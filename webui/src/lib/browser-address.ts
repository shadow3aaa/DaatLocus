/** Omnibox input is either an explicit web URL, a hostname, or a search. */
export function browserDestination(value: string, engine = "https://www.google.com/search?q="): string | null {
  const input=value.trim();if(!input)return null;
  if (/^https?:\/\//i.test(input)) {
    try {const url=new URL(input);return url.hostname && !url.username && !url.password ? url.href : null;} catch {return null;}
  }
  if (/^[a-z][a-z\d+.-]*:/i.test(input) && !/^[\w.-]+:\d+(?:[/?#]|$)/.test(input)) return null;
  if (!/\s/.test(input) && /^(?:localhost(?::\d+)?|\[[\da-f:]+\](?::\d+)?|(?:[\p{L}\d-]+\.)+[\p{L}\d-]+(?::\d+)?)(?:[/?#].*)?$/iu.test(input)) {
    try{return new URL(`${/^localhost(?::|\/|$)|^127\./.test(input)?"http":"https"}://${input}`).href;}catch{return null;}
  }
  return engine+encodeURIComponent(input);
}
