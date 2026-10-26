import {expect,test} from "bun:test";
import {addressSuggestions} from "../src/lib/browser-suggestions";
import type {BrowserSuggestions} from "../src/lib/browser-workspace";
const engine="https://www.google.com/search?q=";
const data:BrowserSuggestions={searches:[{id:"query",query:"WebView guide",time:1}],bookmarks:[{id:"favorite",title:"WebView guide",url:"https://example.com/guide",time:1}],history:[{id:"visit",title:"WebView guide",url:"https://example.com/guide",time:1},{id:"old",title:"Other guide",url:"https://example.org/guide",time:0}]};
const pages=[{tab:"open",title:"WebView reference",url:"https://example.net"}];
test("blank omnibox prefers recent searches and deduplicates a bookmarked visit",()=>{
 const items=addressSuggestions("",null,data,pages,engine);
 expect(items[0].kind).toBe("search_history");expect(items.filter(item=>item.value==="https://example.com/guide")).toHaveLength(1);
 expect(items.find(item=>item.value==="https://example.com/guide")?.kind).toBe("bookmark");
});
test("filters keep matching local sources separate and open tabs keep their identity",()=>{
 expect(addressSuggestions("webview","history",data,pages,engine).map(item=>item.kind)).toEqual(["search_history","history"]);
 expect(addressSuggestions("WEBVIEW","bookmarks",data,pages,engine).map(item=>item.id)).toEqual(["favorite"]);
 expect(addressSuggestions("reference","tabs",data,pages,engine)[0].tab).toBe("open");
 expect(addressSuggestions("missing", "tabs",data,pages,engine)).toEqual([]);
 expect(addressSuggestions("javascript:alert(1)",null,{history:[],bookmarks:[],searches:[]},[],engine)).toEqual([]);
});
