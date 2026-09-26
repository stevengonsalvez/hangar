// jest stand-in for expo-file-system's Paths and Directory: an in-memory tree
// under a fake document directory.
const made = new Set<string>();

export class Directory {
  readonly uri: string;
  constructor(base: { uri: string } | string, ...parts: string[]) {
    const root = typeof base === "string" ? base : base.uri;
    this.uri = [root.replace(/\/$/, ""), ...parts].join("/") + "/";
  }
  get exists() {
    return made.has(this.uri);
  }
  create(_opts?: { intermediates?: boolean; idempotent?: boolean }) {
    made.add(this.uri);
  }
}

export const Paths = { document: new Directory("file:///data/user/0/com.stevengonsalvez.ainb.mobile/files") };
Paths.document.create();

export function __resetFileSystemMock() {
  made.clear();
  Paths.document.create();
}
