// Tiny line-based highlighter for the TS / TSX / SQL snippets on this page.
// Not a real parser: good enough for short, hand-written marketing snippets.

const KEYWORDS = [
  "import", "from", "export", "default", "async", "await", "const", "let", "return",
  "function", "if", "else", "new", "type", "interface", "extends", "as", "for", "of",
  "null", "true", "false", "throw",
  "SELECT", "FROM", "WHERE", "AND", "ORDER", "BY", "DESC", "LIMIT", "CREATE", "TABLE",
  "INDEX", "ON", "NOT", "NULL", "PRIMARY", "KEY", "REFERENCES", "DEFAULT", "CHECK",
  "JOIN", "GROUP", "COUNT", "ANY", "IN",
];

const RULES: [RegExp, string][] = [
  [/^(\/\/|--\s|#\s).*/, "com"],
  [/^(["'`])(?:\\.|(?!\1).)*\1/, "str"],
  [/^\d+(\.\d+)?\b/, "num"],
  [/^<\/?[A-Za-z][\w.]*/, "tag"],
  [new RegExp(`^(?:${KEYWORDS.join("|")})\\b`), "kw"],
  [/^[A-Z][A-Za-z0-9_]*/, "type"],
  [/^[a-zA-Z_$][\w$]*(?=\s*\()/, "fn"],
  [/^[a-zA-Z_$][\w$]*/, "id"],
  [/^\s+/, "ws"],
  [/^./, "punct"],
];

export type Token = { t: string; v: string };

export function tokenize(line: string): Token[] {
  const out: Token[] = [];
  let rest = line;
  while (rest.length) {
    for (const [re, t] of RULES) {
      const m = rest.match(re);
      if (m) {
        out.push({ t, v: m[0] });
        rest = rest.slice(m[0].length);
        break;
      }
    }
  }
  return out;
}

export function Code({ line }: { line: string }) {
  if (!line) return <>{" "}</>;
  return (
    <>
      {tokenize(line).map((tok, i) =>
        tok.t === "ws" ? tok.v : <span key={i} className={`tk-${tok.t}`}>{tok.v}</span>,
      )}
    </>
  );
}
