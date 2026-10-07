import type { PointerEvent as ReactPointerEvent } from "react";
import { trackPointer } from "../drag";
import { setWidth, usePrefs } from "../prefs";
import type { MachineInfo, SessionState } from "../types";
import { ApprovalList } from "./approvals";
import { SidebarSection } from "./sidebar-section";
import { TokenUsage } from "./token-usage";
import { UsagePanel } from "./usage-panel";

/** Narrowest and widest a sidebar may be dragged to. */
const WIDTH_RANGE = [180, 640] as const;

/** Handle on a sidebar's inner border; dragging it changes that sidebar's width. */
export const SidebarResizer = ({ side }: { side: "left" | "right" }) => {
  const prefs = usePrefs();
  const onPointerDown = (e: ReactPointerEvent) => {
    e.preventDefault();
    const start = e.clientX;
    const width = side === "left" ? prefs.leftWidth : prefs.rightWidth;
    const move = (ev: PointerEvent) => {
      const delta = side === "left" ? ev.clientX - start : start - ev.clientX;
      setWidth(side, Math.min(WIDTH_RANGE[1], Math.max(WIDTH_RANGE[0], width + delta)));
    };
    trackPointer(move);
  };
  return <div className="sidebar-resizer" onPointerDown={onPointerDown} />;
};

type Props = {
  sessions: SessionState[];
  machines: MachineInfo[];
  onShowApproval: (key: string) => void;
};

/** Right sidebar: pending approvals, rate limits and token usage. */
export const RightSidebar = ({ sessions, machines, onShowApproval }: Props) => {
  const prefs = usePrefs();
  return (
    <aside className="right-sidebar">
      <SidebarSection title="Approvals" foldKey="approvals" prefs={prefs}>
        <ApprovalList sessions={sessions} onShow={onShowApproval} />
      </SidebarSection>
      <SidebarSection title="Usage" foldKey="usage" prefs={prefs}>
        <UsagePanel sessions={sessions} machines={machines} />
      </SidebarSection>
      <SidebarSection title="Tokens" foldKey="tokens" prefs={prefs}>
        <TokenUsage sessions={sessions} machines={machines} />
      </SidebarSection>
    </aside>
  );
};
