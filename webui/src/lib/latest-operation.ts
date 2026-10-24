/** Keep one in-flight operation and only the most recent pending value. */
export function latestOperation<T>(apply: (value:T)=>Promise<unknown>) {
  let running=false;
  let pending:{value:T;resolve:()=>void;reject:(error:unknown)=>void}[]=[];
  async function drain(){
    if(running)return;
    running=true;
    while(pending.length){
      const batch=pending;pending=[];
      try{await apply(batch[batch.length-1].value);batch.forEach(item=>item.resolve());}
      catch(error){batch.forEach(item=>item.reject(error));}
    }
    running=false;
  }
  return (value:T)=>new Promise<void>((resolve,reject)=>{pending.push({value,resolve,reject});void drain();});
}
