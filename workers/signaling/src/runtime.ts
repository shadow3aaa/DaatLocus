/** Portable relay boundary. No Actor state or execution authority belongs here. */
export interface RelayEnv { GROUP_ROOTS?:string; REVOKED_STAGES:string }
export interface RelaySocket {
  readonly readyState:number;
  send(data:string|ArrayBuffer):unknown;
  close(code?:number,reason?:string):void;
  serializeAttachment(value:unknown):void;
  deserializeAttachment():any;
}
export interface RelayContext {
  getWebSockets():RelaySocket[];
  blockConcurrencyWhile<T>(callback:()=>Promise<T>):Promise<T>;
  storage:{get<T>(key:string):Promise<T|undefined>;put<T>(key:string,value:T):Promise<unknown>;setAlarm(time:number):Promise<unknown>};
}
