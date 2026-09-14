import { createMemo } from "solid-js";
import { marked } from "marked";
import DOMPurify from "dompurify";

marked.setOptions({ gfm: true, breaks: true });

// Render markdown text as sanitized HTML with Tailwind typography styling.
// `dark:prose-invert` remaps the prose palette for dark mode.
export default function Markdown(props: { text: string; class?: string }) {
  const html = createMemo(() => DOMPurify.sanitize(marked.parse(props.text ?? "") as string));
  return (
    <div
      class={`prose prose-sm prose-neutral max-w-none dark:prose-invert ${props.class ?? ""}`}
      innerHTML={html()}
    />
  );
}
