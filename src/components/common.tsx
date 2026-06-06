import { ChevronRight, type LucideIcon } from "lucide-react";

export function Avatar({ name, src, size }: { name: string; src?: string; size?: "large" }) {
  return (
    <span className={size === "large" ? "avatar-chip avatar-large" : "avatar-chip"}>
      {src ? <img alt="" src={src} /> : (name || "?").slice(0, 1).toUpperCase()}
    </span>
  );
}

export function MenuRow({
  icon: Icon,
  label,
  value,
  onClick,
  iconColor
}: {
  icon: LucideIcon;
  label: string;
  value?: string;
  onClick?: () => void;
  iconColor?: "green" | "orange" | "blue" | "red" | "indigo" | "yellow" | "cyan" | "peach" | "purple" | "neutral" | "primary";
}) {
  return (
    <button className="menu-row" onClick={onClick} type="button">
      <span className={`row-icon ${iconColor || "primary"}`}><Icon size={17} /></span>
      <span className="row-label">{label}</span>
      {value ? <span className="row-value">{value}</span> : null}
      <ChevronRight size={16} strokeWidth={2} style={{ color: "var(--text-3)", opacity: 0.6 }} />
    </button>
  );
}
