import { DurableObject } from "cloudflare:workers";
import { RelayGroup } from "./group";
import { AccountRelay } from "./accounts";
import type { RelayEnv } from "./runtime";
interface Env extends RelayEnv {GROUPS:DurableObjectNamespace<SignalGroup>;ACCOUNTS:DurableObjectNamespace<AccountRoom>}
export default {
  async fetch(request:Request,env:Env):Promise<Response>{
    const path=new URL(request.url).pathname;
    if(request.method==="GET"&&path==="/health")return Response.json({service:"daat-locus-relay",protocol:2});
    if(request.method!=="GET"||request.headers.get("Upgrade")?.toLowerCase()!=="websocket")return new Response("Not found",{status:404});
    const match=path.match(/^\/(connect|accounts)\/([0-9a-f]{64})$/);if(!match)return new Response("Not found",{status:404});
    const namespace=match[1]==="connect"?env.GROUPS:env.ACCOUNTS;
    return namespace.get(namespace.idFromName(match[2])).fetch(request);
  }
};
export class SignalGroup extends DurableObject<Env>{
  private relay:RelayGroup;
  constructor(ctx:DurableObjectState,env:Env){super(ctx,env);this.relay=new RelayGroup(ctx,env);}
  async fetch(request:Request){
    if(this.ctx.getWebSockets().length>=64)return new Response("Busy",{status:503});
    const pair=new WebSocketPair();this.ctx.acceptWebSocket(pair[1]);
    await this.relay.connect(pair[1],new URL(request.url).pathname.split("/").pop()!);
    return new Response(null,{status:101,webSocket:pair[0]});
  }
  webSocketMessage(socket:WebSocket,message:string|ArrayBuffer){return this.relay.webSocketMessage(socket,message);}
  webSocketClose(socket:WebSocket){this.relay.webSocketClose(socket);}
  webSocketError(socket:WebSocket){this.relay.webSocketError(socket);}
  alarm(){return this.relay.alarm();}
}
export class AccountRoom extends DurableObject<Env>{
  private relay:AccountRelay;
  constructor(ctx:DurableObjectState,env:Env){super(ctx,env);this.relay=new AccountRelay(ctx,env);}
  async fetch(request:Request){
    if(this.ctx.getWebSockets().length>=64)return new Response("Busy",{status:503});
    const pair=new WebSocketPair();this.ctx.acceptWebSocket(pair[1]);
    await this.relay.connect(pair[1],new URL(request.url).pathname.split("/").pop()!);
    return new Response(null,{status:101,webSocket:pair[0]});
  }
  webSocketMessage(socket:WebSocket,message:string|ArrayBuffer){return this.relay.webSocketMessage(socket,message);}
  webSocketClose(socket:WebSocket){this.relay.webSocketClose(socket);}
  webSocketError(socket:WebSocket){this.relay.webSocketError(socket);}
  alarm(){return this.relay.alarm();}
}
