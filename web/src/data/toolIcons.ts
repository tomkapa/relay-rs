import type { LucideIcon } from "lucide-react";
import {
  BookOpen,
  Database,
  FileBox,
  FilePlus,
  Layers,
  MessageSquare,
  Rows3,
  Search,
  Send,
  Settings,
  Users,
} from "lucide-react";

/** Best-effort lucide icon per tool. Match on the suffix (`pages.search` →
 *  `search`) first, then on a known prefix (`pages.*` → `BookOpen`). Falls
 *  back to `null` so callers can render their own default. */
export function iconForTool(remoteName: string): LucideIcon | null {
  const lower = remoteName.toLowerCase();
  const suffix = lower.split(/[._/]/).pop() ?? lower;
  const head = lower.split(/[._/]/)[0] ?? lower;

  const bySuffix: Record<string, LucideIcon> = {
    search: Search,
    create: FilePlus,
    append: Rows3,
    query: Database,
    list: Layers,
    send: Send,
    update: Settings,
  };
  if (bySuffix[suffix]) return bySuffix[suffix];

  const byHead: Record<string, LucideIcon> = {
    pages: BookOpen,
    page: BookOpen,
    databases: Database,
    database: Database,
    blocks: Rows3,
    block: Rows3,
    comments: MessageSquare,
    comment: MessageSquare,
    users: Users,
    user: Users,
    files: FileBox,
    file: FileBox,
  };
  if (byHead[head]) return byHead[head];

  return null;
}
