import { Fragment, type ReactNode } from "react";

// Match `@name`, `@name-with-dash`, `@name_with_underscore`. The leading
// boundary is start-of-string or whitespace so an email like `a@b.com` is
// not highlighted.
const MENTION_RE_SRC = String.raw`(^|\s)(@[\w][\w-]*)`;

/** Fresh stateful regex per call — sharing a global `g` regex across modules
 *  is a `lastIndex` footgun. */
function mentionRegex(): RegExp {
  return new RegExp(MENTION_RE_SRC, "g");
}

export function forEachMention(
  text: string,
  cb: (tag: string, tagStart: number) => void,
): void {
  if (!text) return;
  const re = mentionRegex();
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) {
    const lead = m[1] ?? "";
    const tag = m[2] ?? "";
    cb(tag, m.index + lead.length);
  }
}

export function renderMentions(text: string): ReactNode {
  if (!text) return text;
  const out: ReactNode[] = [];
  let last = 0;
  forEachMention(text, (tag, tagStart) => {
    if (tagStart > last) out.push(text.slice(last, tagStart));
    out.push(
      <span
        key={tagStart}
        className="font-semibold text-[var(--color-moss)]"
      >
        {tag}
      </span>,
    );
    last = tagStart + tag.length;
  });
  if (last < text.length) out.push(text.slice(last));
  return <Fragment>{out}</Fragment>;
}

/** Prepend `@name ` to `text` unless it already starts with that mention. */
export function prefixMention(text: string, name: string | null | undefined): string {
  if (!name) return text;
  if (text.startsWith(`@${name}`)) return text;
  return `@${name} ${text}`;
}
