import {request} from "../src/lib/actor-api";
export function accountRequest<T>(path:string,method="GET",body?:Record<string,unknown>){return request<T>(path,{method,body});}
