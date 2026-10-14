import { accountError, afterRejectedProof } from "./account-errors";
import { request } from "./actor-api";
import { accountRequest } from "./desktop-account";
import initialize, { Registration, Login } from "@/generated/account-pake/daat_locus_auth";
let ready:Promise<unknown>|undefined;
const prepare=()=>ready??=initialize().catch(()=>{ready=undefined;throw accountError("login_client_unavailable");});
const encode=(value:Uint8Array)=>btoa(Array.from(value,byte=>String.fromCharCode(byte)).join("")).replace(/\+/g,"-").replace(/\//g,"_").replace(/=+$/g,"");
const decode=(value:string)=>Uint8Array.from(atob(value.replace(/-/g,"+").replace(/_/g,"/")),char=>char.charCodeAt(0));
export type AccountInfo={registered:boolean;account_id:string|null;name:string|null;can_create:boolean;signaling_origin:string|null};
export const accountInfo=()=>accountRequest<AccountInfo>("/v1/account");
export const saveAccountConnection=(signaling_origin:string|null)=>accountRequest<void>("/v1/account/connection","PUT",{signaling_origin});
type Start={id:string;response:string;context?:string;remote?:boolean;account_id?:string;device_id?:string};
export async function loginAccount(name:string,password:string) {
  await prepare();const bytes=new TextEncoder().encode(password);const exclude:string[]=[];
  try {
    for(let attempt=0;attempt<8;attempt++) {
      const client=new Login(bytes);
      try {
        let reply:Start;
        try {reply=await accountRequest<Start>("/v1/account/login/start","POST",{name,request:encode(client.request()),exclude});}
        catch(error){throw afterRejectedProof(error,exclude);}
        if(!reply.context)throw accountError("login_protocol_error");
        let response:Uint8Array,context:Uint8Array;
        try {response=decode(reply.response);context=decode(reply.context);} catch {throw accountError("login_protocol_error");}
        let message:Uint8Array;
        try { message=client.finish(response,context); }
        catch {if(reply.remote&&reply.device_id){await request<void>("/v1/account/login/cancel",{method:"POST",body:{id:reply.id,message:""}});exclude.push(reply.device_id);continue;}throw accountError("login_invalid_credentials");}
        const result=await request<{import_id:string;payload:string}|undefined>("/v1/account/login/finish",{method:"POST",body:{id:reply.id,message:encode(message)}});
        if(result?.import_id) {
          let record:Uint8Array;
          try {record=client.open(decode(result.payload));} catch {throw accountError("login_import_rejected");}
          try {await accountRequest<void>("/v1/account/login/import","POST",{id:result.import_id,record:encode(record)});} finally{record.fill(0);}
          exclude.length=0;continue;
        }
        return;
      } finally {client.free();}
    }
    throw accountError(exclude.length ? "login_invalid_credentials" : "login_retry_exhausted");
  } finally {bytes.fill(0);}
}
export async function createAccount(name:string,password:string) {
  if(Array.from(password).length<12)throw accountError("login_password_too_short");
  await prepare();const bytes=new TextEncoder().encode(password);let client:Registration|undefined;
  try {
    client=new Registration(bytes);
    const reply=await accountRequest<Start>("/v1/account/registration/start","POST",{name,request:encode(client.request())});
    const message=client.finish(decode(reply.response));
    await accountRequest<void>("/v1/account/registration/finish","POST",{id:reply.id,message:encode(message)});
  } finally {bytes.fill(0);client?.free();}
  await loginAccount(name,password);
}
