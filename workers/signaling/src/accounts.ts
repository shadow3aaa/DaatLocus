import type {RelayContext,RelaySocket,RelayEnv} from "./runtime";
import {verify,membership,bytes,hash,type Device,type Enrollment,type Proof} from "./auth";
interface Attachment {name:string;challenge:string;deadline:number;device?:Device;account?:string;label?:string;expiry?:number;window:number;messages:number}
type Packet={kind:string;[key:string]:unknown};
function require(value:unknown):asserts value{if(!value)throw Error("Invalid account rendezvous");}
function removed(env:RelayEnv){const list=JSON.parse(env.REVOKED_STAGES||"[]") as string[];require(Array.isArray(list)&&list.length<=1024);return new Set(list);}
export class AccountRelay {
  constructor(private ctx:RelayContext,private env:RelayEnv){}
  private active(socket:RelaySocket,now:number){if(socket.readyState!==1)return;const value=socket.deserializeAttachment() as Attachment;return value.device&&value.expiry&&value.expiry>now&&!removed(this.env).has(value.device.stage_id)?value:undefined;}
  async connect(socket:RelaySocket,name:string):Promise<void>{
    const now=Date.now();const challenge=crypto.randomUUID();
    socket.serializeAttachment({name,challenge,deadline:now+30000,window:now,messages:0} satisfies Attachment);
    await this.ctx.storage.setAlarm(now+30000);
    socket.send(JSON.stringify({type:"challenge",nonce:challenge,expires_at_ms:now+30000}));
  }
  async webSocketMessage(socket:RelaySocket,message:string|ArrayBuffer){
    try{
      require(typeof message==="string"&&new TextEncoder().encode(message).length<=64*1024);
      const now=Date.now();let state=socket.deserializeAttachment() as Attachment;
      if(now-state.window>=1000){state.window=now;state.messages=0;}require(++state.messages<=32);socket.serializeAttachment(state);
      const input=JSON.parse(message) as {type:string;device?:Device;proof?:Proof;enrollment?:Enrollment;label?:string;target?:string;nonce?:string;packet?:Packet};
      if(!state.device){
        require(input.type==="register"&&input.device&&input.proof&&state.deadline>now);
        const device=input.device;require(/^[0-9a-f-]{36}$/.test(device.stage_id)&&/^[0-9a-f]{64}$/.test(device.tls_sha256)&&bytes(device.public_key,32).length===32);
        const registration=await verify<{version:number;challenge:string;expires_at_ms:number;name_hash:string;device:Device;account_id:string|null}>(input.proof,"account-rendezvous",device.public_key);
        require(registration.version===1&&registration.challenge===state.challenge&&registration.expires_at_ms>now&&registration.expires_at_ms<=state.deadline&&registration.name_hash===state.name&&registration.device.stage_id===device.stage_id&&registration.device.public_key===device.public_key&&registration.device.tls_sha256===device.tls_sha256);
        let account:string|undefined;let expiry=now+180000;
        if(input.enrollment){
          const enrollment=input.enrollment;const roots=JSON.parse(this.env.GROUP_ROOTS||"{}") as Record<string,string>;
          require(enrollment.root.group_id===await hash(bytes(enrollment.root.public_key,32))&&(!Object.keys(roots).length||roots[enrollment.root.group_id]===enrollment.root.public_key));
          const member=await membership(enrollment,enrollment.root,removed(this.env),now);
          require(member.can_enroll&&member.device.stage_id===device.stage_id&&member.device.public_key===device.public_key&&member.device.tls_sha256===device.tls_sha256&&registration.account_id===enrollment.root.group_id);
          account=enrollment.root.group_id;expiry=member.expires_at_ms;
        }else require(registration.account_id===null);
        require(typeof input.label==="string"&&new TextEncoder().encode(input.label).length<=100);
        for(const previous of this.ctx.getWebSockets())if(previous!==socket&&previous.readyState===1&&(previous.deserializeAttachment() as Attachment)?.device?.stage_id===device.stage_id)previous.close(1000,"Replaced");
        state={...state,device,account,label:input.label,expiry};socket.serializeAttachment(state);
        const peers=this.ctx.getWebSockets().filter(peer=>peer!==socket).map(peer=>this.active(peer,now)).filter(peer=>peer?.account).map(peer=>({device:peer!.device,account_id:peer!.account,label:peer!.label}));
        socket.send(JSON.stringify({type:"registered",peers}));return;
      }
      require(this.active(socket,now)&&input.type==="forward"&&typeof input.target==="string"&&typeof input.nonce==="string"&&/^[0-9a-f-]{36}$/.test(input.nonce)&&input.packet);
      require(state.account?["challenge","accepted","rejected"].includes(input.packet.kind):["start","finish"].includes(input.packet.kind));
      const target=this.ctx.getWebSockets().find(peer=>this.active(peer,now)?.device?.stage_id===input.target);
      if(!target){socket.send(JSON.stringify({type:"unreachable",nonce:input.nonce}));return;}
      const destination=this.active(target,now)!;require(Boolean(destination.account)!==Boolean(state.account));
      target.send(JSON.stringify({type:"forward",source:state.device,nonce:input.nonce,packet:input.packet}));
    }catch{socket.close(1008,"Rejected");}
  }
  async alarm(){const now=Date.now();for(const socket of this.ctx.getWebSockets()){const state=socket.deserializeAttachment() as Attachment;if(state.device?!this.active(socket,now):state.deadline<=now)socket.close(1008,"Expired");}if(this.ctx.getWebSockets().length)await this.ctx.storage.setAlarm(now+30000);}
  webSocketClose(socket:RelaySocket){socket.close(1000,"Closed");}
  webSocketError(socket:RelaySocket){socket.close(1011,"Connection failed");}
}
