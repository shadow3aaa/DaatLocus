import {expect,test} from "bun:test";
import {browserDestination} from "../src/lib/browser-address";
test("omnibox distinguishes navigation, search and prohibited schemes",()=>{
  for(const [input,expected] of [
    [" example.com/docs ","https://example.com/docs"],
    ["localhost:5177/browser","http://localhost:5177/browser"],
    ["how to use DL","https://www.google.com/search?q=how%20to%20use%20DL"],
    ["https://example.com/?q=a","https://example.com/?q=a"],
    ["javascript:alert(1)",null],["file:///private",null],["https://user:secret@example.com",null],["",null],
  ] as const)expect(browserDestination(input)).toBe(expected);
});
