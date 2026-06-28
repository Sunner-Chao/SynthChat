import type { ComponentType, CSSProperties } from "react";
import {
  Bot,
  Brain,
  Cpu,
  MessageSquareText,
  Mic,
  PlugZap,
  Sparkles,
  Volume2,
  type LucideProps
} from "lucide-react";

type PetStartupAwakeningProps = {
  avatarRevealed: boolean;
  exiting: boolean;
};

type StartupNode = {
  id: string;
  label: string;
  icon: ComponentType<LucideProps>;
  style: CSSProperties;
};

const startupNodes: StartupNode[] = [
  { id: "chat", label: "CHAT", icon: MessageSquareText, style: { "--angle": "-118deg", "--counter-angle": "118deg", "--delay": "0ms" } as CSSProperties },
  { id: "agent", label: "AGENT", icon: Bot, style: { "--angle": "-54deg", "--counter-angle": "54deg", "--delay": "90ms" } as CSSProperties },
  { id: "memory", label: "MEM", icon: Brain, style: { "--angle": "3deg", "--counter-angle": "-3deg", "--delay": "180ms" } as CSSProperties },
  { id: "mcp", label: "MCP", icon: PlugZap, style: { "--angle": "62deg", "--counter-angle": "-62deg", "--delay": "270ms" } as CSSProperties },
  { id: "stt", label: "STT", icon: Mic, style: { "--angle": "124deg", "--counter-angle": "-124deg", "--delay": "360ms" } as CSSProperties },
  { id: "tts", label: "TTS", icon: Volume2, style: { "--angle": "178deg", "--counter-angle": "-178deg", "--delay": "450ms" } as CSSProperties }
];

const waveHeights = [34, 52, 28, 66, 44, 74, 38, 58, 30, 64, 42, 54];

const particlePalette = ["#22d3ee", "#f472b6", "#a3e635", "#facc15", "#38bdf8"];

const startupParticles = Array.from({ length: 34 }, (_, index) => {
  const angle = (index * 137.5) % 360;
  const distance = 74 + (index % 7) * 19;
  const size = 2 + (index % 4);
  return {
    id: index,
    style: {
      "--particle-angle": `${angle}deg`,
      "--particle-counter-angle": `${-angle}deg`,
      "--particle-distance": `${distance}px`,
      "--particle-start-distance": `${Math.round(distance * -0.18)}px`,
      "--particle-pre-distance": `${Math.round(distance * -0.22)}px`,
      "--particle-size": `${size}px`,
      "--particle-delay": `${index * 38}ms`,
      "--particle-color": particlePalette[index % particlePalette.length]
    } as CSSProperties
  };
});

export function PetStartupAwakening({ avatarRevealed, exiting }: PetStartupAwakeningProps) {
  return (
    <div
      className={`pet-startup-awakening${avatarRevealed ? " has-avatar" : ""}${exiting ? " is-exiting" : ""}`}
      aria-hidden="true"
    >
      <div className="pet-startup-matrix" />
      <div className="pet-startup-aurora" />
      <div className="pet-startup-particle-field">
        {startupParticles.map((particle) => (
          <i key={particle.id} style={particle.style} />
        ))}
      </div>
      <div className="pet-startup-orbit">
        <span className="pet-startup-portal" />
        <span className="pet-startup-ring is-wide" />
        <span className="pet-startup-ring is-tight" />
        <span className="pet-startup-scanline" />

        <div className="pet-startup-core">
          <Sparkles size={18} strokeWidth={2.2} />
          <strong>SynthChat</strong>
          <span>Live2D Awakening</span>
        </div>

        <div className="pet-startup-nodes">
          {startupNodes.map((node) => {
            const Icon = node.icon;
            return (
              <span className={`pet-startup-node is-${node.id}`} key={node.id} style={node.style}>
                <Icon size={15} strokeWidth={2.2} />
                <b>{node.label}</b>
              </span>
            );
          })}
        </div>

        <div className="pet-startup-waveform">
          <Cpu size={14} strokeWidth={2.1} />
          <span className="pet-startup-wave-bars">
            {waveHeights.map((height, index) => (
              <i
                key={`${height}-${index}`}
                style={{ "--bar-height": `${height}%`, "--bar-delay": `${index * 46}ms` } as CSSProperties}
              />
            ))}
          </span>
        </div>
      </div>
    </div>
  );
}
