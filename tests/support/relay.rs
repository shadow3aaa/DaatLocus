//! Isolated in-process relay for device protocol tests. Registration is a mock;
//! production relay authentication is exercised by the Worker/standalone suites.
//! Device streams still perform the production mutual TLS and durable handshake.
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_tungstenite::tungstenite::Message;

#[derive(Clone)]
struct Device {
    socket: uuid::Uuid,
    sender: mpsc::Sender<Message>,
    profile: Value,
    account: Option<String>,
    label: String,
}
#[derive(Default)]
struct State {
    devices: HashMap<(String, String), Device>,
    blocked: HashSet<String>,
    channels: HashMap<uuid::Uuid, (String, String, String)>,
}
struct CapturePath(Arc<Mutex<String>>);
impl tokio_tungstenite::tungstenite::handshake::server::Callback for CapturePath {
    // The upstream callback fixes this Response error type; boxing is not permitted.
    #[allow(clippy::result_large_err)]
    fn on_request(
        self,
        request: &tokio_tungstenite::tungstenite::handshake::server::Request,
        response: tokio_tungstenite::tungstenite::handshake::server::Response,
    ) -> Result<
        tokio_tungstenite::tungstenite::handshake::server::Response,
        tokio_tungstenite::tungstenite::handshake::server::ErrorResponse,
    > {
        *self.0.lock() = request.uri().path().to_owned();
        Ok(response)
    }
}
pub struct Relay {
    pub endpoint: String,
    task: JoinHandle<()>,
    #[allow(dead_code)] // Used by partition tests; shared HTTP fixtures only relay.
    state: Arc<Mutex<State>>,
}
impl Drop for Relay {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn send(device: &Device, value: Value) {
    let _ = device
        .sender
        .try_send(Message::Text(value.to_string().into()));
}
impl Relay {
    #[allow(dead_code)]
    pub fn isolate(&self, id: impl ToString) {
        let id = id.to_string();
        let mut state = self.state.lock();
        state.blocked.insert(id.clone());
        for ((_, device), entry) in &state.devices {
            if device == &id {
                let _ = entry.sender.try_send(Message::Close(None));
            }
        }
    }
    #[allow(dead_code)]
    pub fn allow(&self, id: impl ToString) {
        self.state.lock().blocked.remove(&id.to_string());
    }
    pub async fn new() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(State::default()));
        let shared_state = state.clone();
        let task = tokio::spawn(async move {
            let mut jobs = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted=listener.accept()=>{
                        let Ok((stream,_))=accepted else {break;};let shared=state.clone();
                        jobs.spawn(async move {
                            let path=Arc::new(Mutex::new(String::new()));let route=path.clone();
                            let mut socket=match tokio_tungstenite::accept_hdr_async(stream,CapturePath(route)).await {Ok(socket)=>socket,Err(_)=>return};
                            let group=path.lock().clone();let accounts=group.starts_with("/accounts/");
                            let nonce=uuid::Uuid::new_v4();
                            let mut challenge=json!({"type":"challenge","nonce":nonce,"expires_at_ms":chrono::Utc::now().timestamp_millis()+30000,"protocol":2});if accounts{challenge.as_object_mut().unwrap().remove("protocol");}
                            if socket.send(Message::Text(challenge.to_string().into())).await.is_err(){return;}
                            let Some(Ok(Message::Text(text)))=socket.next().await else{return;};
                            let Ok(value)=serde_json::from_str::<Value>(&text) else{return;};
                            let profile=if accounts {value["device"].clone()}else{
                                let Some(proof)=value["enrollment"]["chain"].as_array().and_then(|chain|chain.last()) else{return;};
                                use base64::Engine;
                                let Ok(payload)=base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(proof["payload"].as_str().unwrap_or("")) else{return;};
                                let Ok(member)=serde_json::from_slice::<Value>(&payload) else{return;};member["device"].clone()
                            };
                            let Some(id)=profile["stage_id"].as_str().map(str::to_owned) else{return;};
                            let instance=uuid::Uuid::new_v4();let (tx,mut outgoing)=mpsc::channel::<Message>(256);
                            {
                                let mut state=shared.lock();
                                if state.blocked.contains(&id){return;}
                                let peers=state.devices.iter().filter(|((account,device),_)|account==&group&&device!=&id).filter_map(|((_,id),device)|{
                                    if accounts {device.account.as_ref().map(|account|json!({"device":device.profile,"account_id":account,"label":device.label}))}else{Some(json!(id))}
                                }).collect::<Vec<_>>();
                                let device=Device{socket:instance,sender:tx.clone(),profile:profile.clone(),account:value["enrollment"]["root"]["group_id"].as_str().map(str::to_owned),label:value["label"].as_str().unwrap_or("").to_owned()};
                                send(&device,if accounts{json!({"type":"registered","peers":peers})}else{json!({"type":"registered","protocol":2,"peers":peers})});
                                if let Some(old)=state.devices.insert((group.clone(),id.clone()),device){let _=old.sender.try_send(Message::Close(None));}
                                for ((account,peer),device) in &state.devices {if !accounts&&account==&group&&peer!=&id {send(device,json!({"type":"presence","stage_id":id,"online":true}));}}
                            }
                            loop {
                                tokio::select! {
                                    frame=outgoing.recv()=>{let Some(frame)=frame else{break;};if socket.send(frame).await.is_err(){break;}},
                                    frame=socket.next()=>{
                                        let Some(Ok(frame))=frame else {break;};
                                        match frame {
                                            Message::Text(text)=>{
                                                let Ok(value)=serde_json::from_str::<Value>(&text) else {break;};
                                                let mut state=shared.lock();
                                                if state.devices.get(&(group.clone(),id.clone())).is_none_or(|device|device.socket!=instance){break;}
                                                let kind=value["type"].as_str().unwrap_or("");
                                                if accounts {
                                                    let target=value["target"].as_str().unwrap_or("").to_owned();
                                                    if let Some(remote)=state.devices.get(&(group.clone(),target)) {send(remote,json!({"type":"forward","source":profile,"nonce":value["nonce"],"packet":value["packet"]}));}
                                                    else{let _=tx.try_send(Message::Text(json!({"type":"unreachable","nonce":value["nonce"]}).to_string().into()));}continue;
                                                }
                                                if kind=="revoke" {for ((account,_),device) in &state.devices {if account==&group {send(device,json!({"type":"revocations","records":[value["record"]]}));}}continue;}
                                                let Some(channel)=value["channel"].as_str().and_then(|id|uuid::Uuid::parse_str(id).ok()) else{break;};
                                                if kind=="open" {
                                                    let target=value["target"].as_str().unwrap_or("").to_owned();
                                                    if let Some(remote)=state.devices.get(&(group.clone(),target.clone())) {
                                                        send(remote,json!({"type":"incoming","channel":channel,"source":id}));
                                                        state.channels.insert(channel,(group.clone(),id.clone(),target));
                                                    }else {let _=tx.try_send(Message::Text(json!({"type":"closed","channel":channel}).to_string().into()));}
                                                }else if let Some((account,a,b))=state.channels.get(&channel).cloned() {
                                                    if account!=group || id!=a&&id!=b {break;}
                                                    let other=if id==a{b}else{a};
                                                    if let Some(remote)=state.devices.get(&(group.clone(),other)) {send(remote,json!({"type":if kind=="accept"{"opened"}else{"closed"},"channel":channel}));}
                                                    if kind=="close"{state.channels.remove(&channel);}
                                                }
                                            },
                                            Message::Binary(bytes)=>{
                                                if bytes.len()<18 {break;}
                                                let Ok(channel)=uuid::Uuid::from_slice(&bytes[2..18]) else{break;};
                                                let state=shared.lock();
                                                let Some((account,a,b))=state.channels.get(&channel) else{continue;};
                                                if account!=&group||(&id!=a&&&id!=b){break;}
                                                let other=if &id==a{b}else{a};
                                                if let Some(remote)=state.devices.get(&(group.clone(),other.clone())){let _=remote.sender.try_send(Message::Binary(bytes));}
                                            },
                                            Message::Ping(bytes)=>{if socket.send(Message::Pong(bytes)).await.is_err(){break;}},
                                            Message::Close(_)=>break,
                                            _=>{},
                                        }
                                    }
                                }
                            }
                            let mut state=shared.lock();
                            if state.devices.get(&(group.clone(),id.clone())).is_some_and(|device|device.socket==instance){
                                state.devices.remove(&(group.clone(),id.clone()));
                                let closed=state.channels.iter().filter(|(_, (account,a,b))|account==&group&&(a==&id||b==&id)).map(|(id,_)|*id).collect::<Vec<_>>();
                                for channel in closed {if let Some((account,a,b))=state.channels.remove(&channel){let other=if a==id{b}else{a};if let Some(device)=state.devices.get(&(account,other)){send(device,json!({"type":"closed","channel":channel}));}}}
                                for ((account,_),device) in &state.devices {if !accounts&&account==&group {send(device,json!({"type":"presence","stage_id":id,"online":false}));}}
                            }
                        });
                    },
                    _=jobs.join_next(),if !jobs.is_empty()=>{},
                }
            }
        });
        Self {
            endpoint,
            task,
            state: shared_state,
        }
    }
}

pub async fn wait(check: impl Fn() -> bool) -> Result<(), &'static str> {
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        while !check() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .map_err(|_| "relay condition timed out")
}
