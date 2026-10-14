import { type ComponentProps, type ReactNode } from "react";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { SettingsRow } from "./structure";
import {
  Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue,
} from "@/components/ui/select";

export function errorText(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

export function StatusNote({ text, error = false }: { text: string | null; error?: boolean }) {
  return text ? <Alert variant={error ? "destructive" : "default"} className="dl-settings-note"><AlertDescription>{text}</AlertDescription></Alert> : null;
}

export function TextField({ id, label, ...props }: ComponentProps<typeof Input> & {
  id: string; label: string;
}) {
  return <SettingsRow id={id} label={label}><Input id={id} {...props} /></SettingsRow>;
}

export function AreaField({ id, label, ...props }: ComponentProps<typeof Textarea> & {
  id: string; label: string;
}) {
  return <SettingsRow id={id} label={label} stacked><Textarea id={id} {...props} /></SettingsRow>;
}

export function Picker({ id, label, value, onChange, options, disabled, action }: {
  id: string; label: string; value: string; onChange: (value: string) => void;
  options: { value: string; label: string }[];  disabled?: boolean; action?: ReactNode;
}) {
  return <SettingsRow id={id} label={label}><Select value={value} onValueChange={onChange} disabled={disabled}>
    <SelectTrigger id={id}><SelectValue /></SelectTrigger>
    <SelectContent><SelectGroup>{options.map((option) => <SelectItem key={option.value} value={option.value}>{option.label}</SelectItem>)}</SelectGroup></SelectContent>
  </Select>{action}</SettingsRow>;
}

export function Choices({ id, label, value, onChange, options, disabled }: {
  id: string; label: string; value: string; onChange: (value: string) => void;
  options: { value: string; label: string }[];  disabled?: boolean;
}) {
  return <SettingsRow label={label} stacked><ToggleGroup id={id} type="single" aria-label={label} value={value} disabled={disabled} className="dl-settings-options" onValueChange={(next) => { if (next) onChange(next); }}>
    {options.map((option) => <ToggleGroupItem key={option.value} value={option.value}>{option.label}</ToggleGroupItem>)}
  </ToggleGroup></SettingsRow>;
}
