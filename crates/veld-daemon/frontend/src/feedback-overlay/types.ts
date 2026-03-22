export type ThreadStatus = "open" | "resolved";
export type UIMode = null | "select-element" | "screenshot" | "draw";

export interface ThreadScope {
  type: "page" | "element" | "global";
  page_url: string;
  selector?: string;
  label?: string;
  position?: { x: number; y: number; width: number; height: number };
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
}

export interface FeedbackEvent {
  seq: number;
  event: string;
  thread_id?: string;
  thread?: Thread;
  message?: Message;
  data?: unknown;
}

/** An HTMLElement with optional veld-specific cleanup/type metadata. */
export interface VeldPopoverElement extends HTMLElement {
  _veldType?: string;
  _veldCleanup?: () => void;
}
