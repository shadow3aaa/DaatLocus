import { Fragment } from "react";

export function HighlightedText({text,query}:{text:string;query:string}) {
  const needle=query.trim().toLowerCase();
  if(!needle)return <>{text}</>;
  const folded=text.toLowerCase(),parts=[];
  let from=0,index=folded.indexOf(needle);
  while(index>=0){
    parts.push(<Fragment key={index}>{text.slice(from,index)}<mark>{text.slice(index,index+needle.length)}</mark></Fragment>);
    from=index+needle.length;index=folded.indexOf(needle,from);
  }
  return <>{parts}{text.slice(from)}</>;
}
