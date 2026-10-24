/** Preview HTTP always targets the local fixture server, even in a native WebView. */
export const serviceFetch:typeof fetch=(input,init)=>fetch(input,init);
export const resourceSource=(path:string)=>path;
export async function downloadResource(path:string,name:string){const link=document.createElement("a");link.href=path;link.download=name;link.click();return true;}
