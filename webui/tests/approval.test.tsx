import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { Approval } from "../src/components/conversation-content";
import type { ApprovalRecord } from "../src/lib/actor-api";

test("only manual approvals render a decision control", () => {
  const record: ApprovalRecord = {
    seq:1,id:"approval",actor_id:"worker",message_id:"input",
    call:{id:"call",name:"files__read_file",arguments:{path:"/reference"}},
    requested:{readable_roots:["/reference"],writable_roots:[],operation_details:null},
    mode:"auto",decision:"pending",reason:"private review",created_at_ms:1,resolved_at_ms:null,
  };
  for (const decision of ["pending", "approved", "denied", "cancelled"] as const) {
    expect(renderToStaticMarkup(<Approval record={{...record,decision}} canManage onResolved={()=>{}} />)).toBe("");
  }
  const manual=renderToStaticMarkup(<Approval record={{...record,mode:"ask"}} canManage onResolved={()=>{}} />);
  expect(manual).toContain('type="button"');
  expect(manual).not.toContain('type="checkbox"');
  expect(manual).not.toContain(record.reason);
});
