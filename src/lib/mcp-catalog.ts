/**
 * Curated catalog of well-known MCP servers for one-click add.
 *
 * Each entry is a *template*: a launch spec (`command` + `args`) plus a
 * declaration of which environment variables the server needs. The values for
 * those vars are ALWAYS user-supplied at add-time — this catalog never ships a
 * token, key, or secret. `args` may include a `{{PLACEHOLDER}}` the user fills
 * in (e.g. an allowed root path for the filesystem server).
 *
 * Sources: the official `@modelcontextprotocol/server-*` reference servers and
 * a few widely-used community servers. Commands use `npx -y` (Node) or `uvx`
 * (Python) so nothing has to be pre-installed.
 */

/** A required environment variable for a catalog server. */
export interface CatalogEnvVar {
  /** The env var name passed to the spawned process (e.g. GITHUB_TOKEN). */
  key: string;
  /** Short human label shown above the input. */
  label: string;
  /** Placeholder / hint shown in the input (never a real value). */
  placeholder: string;
  /** Where to obtain the value, shown as a hint under the field. */
  hint?: string;
  /** Mask the input (tokens/keys). Defaults to true for anything secret. */
  secret?: boolean;
}

/** An argument the user must fill in before adding (e.g. a path). */
export interface CatalogArgPrompt {
  /** Token in `argsTemplate` to replace, e.g. "{{ROOT}}". */
  token: string;
  label: string;
  placeholder: string;
  hint?: string;
}

/** One curated MCP server the user can add in one click. */
export interface CatalogEntry {
  /** Stable catalog id (used for "already added" matching by command+args). */
  id: string;
  name: string;
  description: string;
  /** Loose category for grouping/filtering. */
  category: "files" | "web" | "dev" | "data" | "memory" | "utility";
  command: string;
  /** Args template; tokens like {{ROOT}} are filled from `argPrompts`. */
  argsTemplate: string[];
  /** Env vars the user must supply. Empty for servers that need none. */
  env?: CatalogEnvVar[];
  /** Args the user fills in before adding. */
  argPrompts?: CatalogArgPrompt[];
  /** Project / docs URL for "learn more". */
  homepage?: string;
  /** Extra keywords to widen search (not shown). */
  keywords?: string[];
}

export const MCP_CATALOG: CatalogEntry[] = [
  {
    id: "filesystem",
    name: "Filesystem",
    description:
      "Read, write, and search files within directories you explicitly allow.",
    category: "files",
    command: "npx",
    argsTemplate: ["-y", "@modelcontextprotocol/server-filesystem", "{{ROOT}}"],
    argPrompts: [
      {
        token: "{{ROOT}}",
        label: "Allowed directory",
        placeholder: "/home/you/projects",
        hint: "The server can only touch files under this path. Add more by editing args later.",
      },
    ],
    homepage:
      "https://github.com/modelcontextprotocol/servers/tree/main/src/filesystem",
    keywords: ["files", "fs", "directory", "read", "write"],
  },
  {
    id: "fetch",
    name: "Fetch",
    description:
      "Fetch a URL and return its content as clean markdown for the model.",
    category: "web",
    command: "uvx",
    argsTemplate: ["mcp-server-fetch"],
    homepage:
      "https://github.com/modelcontextprotocol/servers/tree/main/src/fetch",
    keywords: ["http", "url", "web", "scrape", "markdown"],
  },
  {
    id: "github",
    name: "GitHub",
    description:
      "Browse repos, read code, manage issues and pull requests via the GitHub API.",
    category: "dev",
    command: "npx",
    argsTemplate: ["-y", "@modelcontextprotocol/server-github"],
    env: [
      {
        key: "GITHUB_PERSONAL_ACCESS_TOKEN",
        label: "GitHub personal access token",
        placeholder: "ghp_…",
        hint: "Create one at github.com/settings/tokens. Stored locally; never sent anywhere but GitHub.",
        secret: true,
      },
    ],
    homepage:
      "https://github.com/modelcontextprotocol/servers/tree/main/src/github",
    keywords: ["git", "repo", "issues", "pull request", "pr"],
  },
  {
    id: "git",
    name: "Git",
    description:
      "Read, search, and inspect history of a local Git repository.",
    category: "dev",
    command: "uvx",
    argsTemplate: ["mcp-server-git", "--repository", "{{REPO}}"],
    argPrompts: [
      {
        token: "{{REPO}}",
        label: "Repository path",
        placeholder: "/home/you/projects/my-repo",
        hint: "An existing local Git working tree.",
      },
    ],
    homepage:
      "https://github.com/modelcontextprotocol/servers/tree/main/src/git",
    keywords: ["git", "log", "diff", "blame", "commit"],
  },
  {
    id: "memory",
    name: "Memory",
    description:
      "A persistent knowledge graph the model can write facts to and recall later.",
    category: "memory",
    command: "npx",
    argsTemplate: ["-y", "@modelcontextprotocol/server-memory"],
    homepage:
      "https://github.com/modelcontextprotocol/servers/tree/main/src/memory",
    keywords: ["knowledge", "graph", "remember", "recall", "notes"],
  },
  {
    id: "sqlite",
    name: "SQLite",
    description:
      "Query and explore a local SQLite database with read and write tools.",
    category: "data",
    command: "uvx",
    argsTemplate: ["mcp-server-sqlite", "--db-path", "{{DB}}"],
    argPrompts: [
      {
        token: "{{DB}}",
        label: "Database file",
        placeholder: "/home/you/data/app.db",
        hint: "Path to a .db / .sqlite file (created if it does not exist).",
      },
    ],
    homepage:
      "https://github.com/modelcontextprotocol/servers/tree/main/src/sqlite",
    keywords: ["sql", "database", "db", "query"],
  },
  {
    id: "brave-search",
    name: "Brave Search",
    description: "Web and local search results via the Brave Search API.",
    category: "web",
    command: "npx",
    argsTemplate: ["-y", "@modelcontextprotocol/server-brave-search"],
    env: [
      {
        key: "BRAVE_API_KEY",
        label: "Brave Search API key",
        placeholder: "BSA…",
        hint: "Free tier at brave.com/search/api. Stored locally.",
        secret: true,
      },
    ],
    homepage:
      "https://github.com/modelcontextprotocol/servers/tree/main/src/brave-search",
    keywords: ["search", "web", "brave", "query"],
  },
  {
    id: "puppeteer",
    name: "Puppeteer",
    description:
      "Drive a headless Chrome browser — navigate, click, fill forms, screenshot.",
    category: "web",
    command: "npx",
    argsTemplate: ["-y", "@modelcontextprotocol/server-puppeteer"],
    homepage:
      "https://github.com/modelcontextprotocol/servers/tree/main/src/puppeteer",
    keywords: ["browser", "chrome", "automation", "screenshot", "scrape"],
  },
  {
    id: "playwright",
    name: "Playwright",
    description:
      "Cross-browser automation and web testing via Microsoft Playwright.",
    category: "web",
    command: "npx",
    argsTemplate: ["-y", "@playwright/mcp@latest"],
    homepage: "https://github.com/microsoft/playwright-mcp",
    keywords: ["browser", "automation", "testing", "e2e", "chromium"],
  },
  {
    id: "time",
    name: "Time",
    description:
      "Current time and timezone conversions — handy for scheduling answers.",
    category: "utility",
    command: "uvx",
    argsTemplate: ["mcp-server-time"],
    homepage:
      "https://github.com/modelcontextprotocol/servers/tree/main/src/time",
    keywords: ["clock", "timezone", "date", "schedule"],
  },
  {
    id: "everything",
    name: "Everything (reference)",
    description:
      "The reference server exercising every MCP feature — great for testing a setup.",
    category: "utility",
    command: "npx",
    argsTemplate: ["-y", "@modelcontextprotocol/server-everything"],
    homepage:
      "https://github.com/modelcontextprotocol/servers/tree/main/src/everything",
    keywords: ["test", "demo", "reference", "example"],
  },
  {
    id: "postgres",
    name: "PostgreSQL",
    description:
      "Read-only access to a PostgreSQL database — inspect schema and run queries.",
    category: "data",
    command: "npx",
    argsTemplate: [
      "-y",
      "@modelcontextprotocol/server-postgres",
      "{{CONN}}",
    ],
    argPrompts: [
      {
        token: "{{CONN}}",
        label: "Connection string",
        placeholder: "postgresql://user:pass@localhost:5432/db",
        hint: "A standard Postgres URL. Stored locally in your MCP config.",
      },
    ],
    homepage:
      "https://github.com/modelcontextprotocol/servers/tree/main/src/postgres",
    keywords: ["sql", "database", "postgres", "pg", "query"],
  },
];

/** Human label for a catalog category. */
export const CATEGORY_LABELS: Record<CatalogEntry["category"], string> = {
  files: "Files",
  web: "Web",
  dev: "Dev",
  data: "Data",
  memory: "Memory",
  utility: "Utility",
};

/**
 * The fully-resolved args for an entry once `argPrompts` are filled.
 * Tokens with no provided value are left as-is so the user can edit later.
 */
export function resolveArgs(
  entry: CatalogEntry,
  fills: Record<string, string>,
): string[] {
  return entry.argsTemplate.map((arg) => {
    const fill = fills[arg];
    return fill !== undefined && fill.trim().length > 0 ? fill.trim() : arg;
  });
}

/** Case-insensitive match of a query against name/description/keywords. */
export function matchesQuery(entry: CatalogEntry, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  const haystack = [
    entry.name,
    entry.description,
    CATEGORY_LABELS[entry.category],
    ...(entry.keywords ?? []),
  ]
    .join(" ")
    .toLowerCase();
  return haystack.includes(q);
}

/**
 * Whether a configured server matches a catalog entry. We compare the command
 * and the static (non-placeholder) parts of the args template so a filled-in
 * path/token doesn't break the "already added" detection.
 */
export function isEntryAdded(
  entry: CatalogEntry,
  servers: { command: string; args: string[] }[],
): boolean {
  const promptTokens = new Set(
    (entry.argPrompts ?? []).map((p) => p.token),
  );
  // The fixed args that must all be present (placeholders excluded).
  const fixed = entry.argsTemplate.filter((a) => !promptTokens.has(a));
  return servers.some(
    (s) =>
      s.command === entry.command &&
      fixed.every((a) => s.args.includes(a)),
  );
}
