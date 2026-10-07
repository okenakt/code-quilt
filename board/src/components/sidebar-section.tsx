import { ChevronDown, ChevronRight } from "lucide-react";
import type { ReactNode } from "react";
import { toggleFold, type Prefs } from "../prefs";

/** Whether a foldable part is expanded: `folded` lists the keys toggled away from their default. */
export const isExpanded = (prefs: Prefs, key: string, byDefault: boolean) => byDefault !== prefs.folded.includes(key);

export const Fold = ({ open }: { open: boolean }) => <span className="fold">{open ? <ChevronDown size={12} /> : <ChevronRight size={12} />}</span>;

type Props = {
  title: string;
  foldKey: string;
  prefs: Prefs;
  children: ReactNode;
};

/** A foldable section of a sidebar. */
export const SidebarSection = ({ title, foldKey, prefs, children }: Props) => {
  const open = isExpanded(prefs, foldKey, true);
  return (
    <section className="sidebar-section">
      <header className="sidebar-section-header">
        <button className="section-title" onClick={() => toggleFold(foldKey)}>
          <Fold open={open} /> {title}
        </button>
      </header>
      {open && <div className="sidebar-section-body">{children}</div>}
    </section>
  );
};
