export type ThreadStatus = "open" | "resolved";
export type UIMode = null | "select-element" | "screenshot";

export interface ThreadScope {
  type: "page" | "element" | "global";
  page_url: string;
  selector?: string;
  label?: string;
  position?: { x: number; y: number; width: number; height: number };
  /** Visible text of the scoped element, middle-truncated — helps an agent
   *  disambiguate when the CSS selector alone matches ambiguously. */
  element_text?: string;
  source_file?: string;
  source_line?: number;
}

export interface Message {
  id: string;
  body: string;
  author: "human" | "agent";
  created_at: string;
  screenshot?: string | null;
}

export interface Thread {
  id: string;
  scope: ThreadScope;
  status: ThreadStatus;
  messages: Message[];
  created_at: string;
  updated_at: string;
  origin?: string;
  component_trace?: string[] | null;
  viewport_width?: number;
  viewport_height?: number;
  /** Count of messages the human has marked seen — persisted server-side, so
   *  unread state survives a reload. */
  last_human_seen_seq?: number | null;
}

export interface FeedbackEvent {
  seq: number;
  event: string;
  thread_id?: string;
  thread?: Thread;
  message?: Message;
  agent_id?: string;
  data?: unknown;
}

/** An HTMLElement with optional veld-specific cleanup/type metadata. */
export interface VeldPopoverElement extends HTMLElement {
  _veldType?: string;
  _veldCleanup?: () => void;
  /** For a new-comment composer: the pageKey() of the URL it was opened on, so
   *  its persisted draft is cleared against the right page even after a
   *  client-side navigation. */
  _veldPageKey?: string;
}
