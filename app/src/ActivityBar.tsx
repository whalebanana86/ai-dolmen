import { ReactNode } from "react";

export interface Activity {
  key: string;
  /** The button's name, for screen readers. */
  label: string;
  /** Shown on hover; the label when absent. */
  title?: string;
  icon: ReactNode;
  /** The panel it opens is showing. */
  pressed?: boolean;
  disabled?: boolean;
  /** A count on the icon, such as a snapshot's changes. */
  badge?: number;
  onClick: () => void;
}

/**
 * The column of icons down the workspace's left edge, as in VS Code: the
 * panels at the top, and `bottom`'s, such as settings, at the bottom.
 */
export function ActivityBar({ label, top, bottom }: { label: string; top: Activity[]; bottom: Activity[] }) {
  return (
    <nav className="activity-bar" aria-label={label}>
      {top.map(button)}
      <span className="spacer" />
      {bottom.map(button)}
    </nav>
  );
}

function button(activity: Activity) {
  return (
    <button
      key={activity.key}
      className="activity-button"
      onClick={activity.onClick}
      disabled={activity.disabled}
      aria-pressed={activity.pressed}
      aria-label={activity.label}
      // Drawn by the style sheet: the web view doesn't show a title's tooltip.
      data-tooltip={activity.title ?? activity.label}
    >
      {activity.icon}
      {!!activity.badge && <span className="activity-badge">{activity.badge > 99 ? "99+" : activity.badge}</span>}
    </button>
  );
}

function Icon({ children }: { children: ReactNode }) {
  return (
    <svg
      width="18"
      height="18"
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.3"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      {children}
    </svg>
  );
}

/** A database: the schema tree. */
export function DatabaseIcon() {
  return (
    <Icon>
      <ellipse cx="8" cy="3.5" rx="5" ry="2" />
      <path d="M3 3.5v9c0 1.1 2.2 2 5 2s5-.9 5-2v-9" />
      <path d="M3 8c0 1.1 2.2 2 5 2s5-.9 5-2" />
    </Icon>
  );
}

/** A clock: the query history. */
export function HistoryIcon() {
  return (
    <Icon>
      <circle cx="8" cy="8" r="6" />
      <path d="M8 4.5V8l2.5 1.5" />
    </Icon>
  );
}

/** Two tables and a relationship: the ERD. */
export function DiagramIcon() {
  return (
    <Icon>
      <rect x="1.5" y="2" width="6" height="4.5" rx="1" />
      <rect x="8.5" y="9.5" width="6" height="4.5" rx="1" />
      <path d="M4.5 6.5v5h4" />
    </Icon>
  );
}

/** A branch: the schema snapshot in Git. */
export function BranchIcon() {
  return (
    <Icon>
      <circle cx="4.5" cy="3.5" r="1.5" />
      <circle cx="4.5" cy="12.5" r="1.5" />
      <circle cx="11.5" cy="5" r="1.5" />
      <path d="M4.5 5v6" />
      <path d="M11.5 6.5c0 2.5-1.5 3.3-4 3.8-1.6.3-2.6.9-3 1.7" />
    </Icon>
  );
}

/** A window with its bottom panel filled in while it shows: the results. */
export function ResultsIcon({ shown }: { shown: boolean }) {
  return (
    <Icon>
      <rect x="1.5" y="2.5" width="13" height="11" rx="2" />
      <path d="M1.5 9.5h13" />
      {shown && <rect x="2" y="9.5" width="12" height="3.5" rx="1" fill="currentColor" stroke="none" opacity="0.45" />}
    </Icon>
  );
}

/** A gear: the settings. */
export function GearIcon() {
  return (
    <Icon>
      <circle cx="8" cy="8" r="2" />
      <path d="M8 1.5v2M8 12.5v2M1.5 8h2M12.5 8h2M3.4 3.4l1.4 1.4M11.2 11.2l1.4 1.4M3.4 12.6l1.4-1.4M11.2 4.8l1.4-1.4" />
      <circle cx="8" cy="8" r="4.2" />
    </Icon>
  );
}
