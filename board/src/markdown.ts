import { Marked, type TokenizerAndRendererExtension, type Tokens } from "marked";
import type Katex from "katex";
import type Mermaid from "mermaid";

/** Board colors for a Markdown document, which an iframe cannot inherit. */
const style = (): string => {
  const css = getComputedStyle(document.documentElement);
  const v = (name: string) => css.getPropertyValue(name).trim();
  return `body{margin:0;padding:16px 24px;font:14px/1.6 system-ui,sans-serif;color:${v("--text")};background:${v("--bg")}}
a{color:${v("--working")}}code,pre{background:${v("--panel")}}pre{padding:8px 12px;overflow:auto}img,svg{max-width:100%}
table{border-collapse:collapse}td,th{border:1px solid ${v("--line")};padding:4px 8px}blockquote{margin:0;padding-left:12px;border-left:3px solid ${v("--line")};color:${v("--muted")}}
.diagram{text-align:center}math[display=block]{margin:12px 0;font-size:1.15em}`;
};

// Both libraries are large, so they load on the first document that needs them.
let katex: typeof Katex | null = null;
let mermaid: Promise<typeof Mermaid> | null = null;
const loadMermaid = () =>
  (mermaid ??= import("mermaid").then(({ default: m }) => {
    m.initialize({ startOnLoad: false, securityLevel: "strict", theme: "dark" });
    return m;
  }));

// Math becomes MathML, which the browser draws without KaTeX's stylesheet and fonts.
const tex = (text: string, displayMode: boolean): string => katex?.renderToString(text, { displayMode, output: "mathml", throwOnError: false }) ?? text;

const startOf = (i: number) => (i < 0 ? undefined : i);

const blockMath: TokenizerAndRendererExtension = {
  name: "blockMath",
  level: "block",
  start: (src) => startOf(src.search(/^ {0,3}\$\$/m)),
  tokenizer: (src) => {
    const m = /^ {0,3}\$\$([\s\S]+?)\$\$[^\S\n]*(?:\n|$)/.exec(src);
    return m ? { type: "blockMath", raw: m[0], text: m[1].trim() } : undefined;
  },
  renderer: (t) => tex(t.text, true),
};

const inlineMath: TokenizerAndRendererExtension = {
  name: "inlineMath",
  level: "inline",
  start: (src) => startOf(src.indexOf("$")),
  tokenizer: (src) => {
    const m = /^\$\$([^$]+?)\$\$/.exec(src) ?? /^\$(?!\s)((?:\\.|[^\\$\n])+?)(?<!\s)\$(?!\d)/.exec(src);
    return m ? { type: "inlineMath", raw: m[0], text: m[1] } : undefined;
  },
  renderer: (t) => tex(t.text, false),
};

type Code = Tokens.Code & { svg?: string };

// mermaid.render cannot run concurrently, so diagrams are drawn one after another.
let drawing: Promise<unknown> = Promise.resolve();
let diagramId = 0;

const marked = new Marked({
  async: true,
  extensions: [blockMath, inlineMath],
  walkTokens: async (token) => {
    const code = token as Code;
    if (code.type !== "code" || code.lang !== "mermaid") return;
    const m = await loadMermaid();
    const draw = drawing.then(() => m.render(`cq-diagram-${++diagramId}`, code.text));
    drawing = draw.catch(() => undefined);
    // A diagram that does not parse stays as its source.
    code.svg = await draw.then((r) => r.svg).catch(() => undefined);
  },
  renderer: {
    code(token) {
      const { svg, lang, text } = token as Code;
      if (svg) return `<div class="diagram">${svg}</div>`;
      if (lang === "math") return tex(text, true);
      return false;
    },
  },
});

/** Renders Markdown into a standalone document that needs no scripts: mermaid diagrams as SVG and math as MathML. */
export const markdownDocument = async (text: string): Promise<string> => {
  if (!katex && /\$|^ {0,3}(```|~~~)\s*math/m.test(text)) katex = (await import("katex")).default;
  return `<!doctype html><meta charset="utf-8"><style>${style()}</style>${await marked.parse(text)}`;
};
