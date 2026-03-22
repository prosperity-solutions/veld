export interface LogEntry {
  ts: string;
  level: string;
  msg: string;
  stack?: string;
}
