export type ThreadStatus = "open" | "resolved";
export type UIMode = null | "select-element" | "screenshot" | "draw";

export interface ThreadScope {
  type: "page" | "element";
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
  type: string;
  thread_id?: string;
  data?: unknown;
}
