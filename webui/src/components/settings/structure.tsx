import type { ReactNode } from "react";
import { Field, FieldContent, FieldDescription, FieldLabel } from "@/components/ui/field";
import { cn } from "@/lib/utils";
export function SettingsSection({title,children,action}:{title:string;children:ReactNode;action?:ReactNode}) {
  return <section className="dl-setting-section"><div className="dl-setting-section-title"><h3>{title}</h3>{action}</div>{children}</section>;
}
export function SettingsGroup({children,className}:{children:ReactNode;className?:string}) {
  return <div className={cn("dl-setting-group",className)}>{children}</div>;
}
export function SettingsRow({label,id,children,description,stacked=false,className}:{label:string;id?:string;children:ReactNode;description?:string;stacked?:boolean;className?:string}) {
  return <Field orientation={stacked?"vertical":"horizontal"} className={cn("dl-setting-row",stacked&&"dl-setting-row-stacked",className)}><FieldContent>{id ? <FieldLabel htmlFor={id}>{label}</FieldLabel> : <span className="dl-setting-label">{label}</span>}{description && <FieldDescription>{description}</FieldDescription>}</FieldContent><div className="dl-setting-control">{children}</div></Field>;
}
