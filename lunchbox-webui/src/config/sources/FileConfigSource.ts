/**
 * Local files.
 *
 * Uses the File System Access API where it exists (Chromium), which gives a
 * real save-in-place. Everywhere else — Firefox and Safari, as of writing —
 * falls back to a file input for opening and a download for saving, which
 * cannot overwrite the original. The UI reads `canSaveInPlace` to label the
 * button honestly rather than pretending.
 */
import type { ConfigDocument, ConfigSource } from "./ConfigSource";

interface FileSystemFileHandleLike {
  name: string;
  getFile(): Promise<File>;
  createWritable(): Promise<{
    write(data: string): Promise<void>;
    close(): Promise<void>;
  }>;
}

type PickerWindow = Window & {
  showOpenFilePicker?: (options?: unknown) => Promise<FileSystemFileHandleLike[]>;
  showSaveFilePicker?: (options?: unknown) => Promise<FileSystemFileHandleLike>;
};

const PICKER_OPTIONS = {
  types: [
    {
      description: "Lunchbox configuration",
      accept: { "application/toml": [".toml"] },
    },
  ],
};

export const hasFileSystemAccess = (): boolean =>
  typeof window !== "undefined" &&
  typeof (window as PickerWindow).showOpenFilePicker === "function";

export class FileConfigSource implements ConfigSource {
  readonly label = "This computer";

  canSaveInPlace(doc: ConfigDocument): boolean {
    return hasFileSystemAccess() && doc.handle != null;
  }

  async open(): Promise<ConfigDocument | null> {
    const w = window as PickerWindow;
    if (w.showOpenFilePicker) {
      let handles: FileSystemFileHandleLike[];
      try {
        handles = await w.showOpenFilePicker(PICKER_OPTIONS);
      } catch {
        return null; // the user dismissed the picker
      }
      const handle = handles[0];
      if (!handle) return null;
      const file = await handle.getFile();
      return { text: await file.text(), name: handle.name, handle };
    }
    return openViaInput();
  }

  async save(doc: ConfigDocument, text: string): Promise<ConfigDocument> {
    const handle = doc.handle as FileSystemFileHandleLike | undefined;
    if (hasFileSystemAccess() && handle) {
      const writable = await handle.createWritable();
      await writable.write(text);
      await writable.close();
      return { ...doc, text };
    }
    download(doc.name ?? "config.toml", text);
    return { ...doc, text };
  }

  async saveAs(doc: ConfigDocument, text: string): Promise<ConfigDocument | null> {
    const w = window as PickerWindow;
    if (w.showSaveFilePicker) {
      let handle: FileSystemFileHandleLike;
      try {
        handle = await w.showSaveFilePicker({
          ...PICKER_OPTIONS,
          suggestedName: doc.name ?? "config.toml",
        });
      } catch {
        return null;
      }
      const writable = await handle.createWritable();
      await writable.write(text);
      await writable.close();
      return { text, name: handle.name, handle };
    }
    download(doc.name ?? "config.toml", text);
    return { ...doc, text };
  }
}

/** Read a file through a hidden `<input type="file">`. */
function openViaInput(): Promise<ConfigDocument | null> {
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = ".toml,application/toml,text/plain";
    input.style.display = "none";
    // `cancel` is not universally supported; a dismissed dialog simply never
    // resolves, which leaves the editor exactly as it was.
    input.addEventListener("change", async () => {
      const file = input.files?.[0];
      input.remove();
      if (!file) return resolve(null);
      resolve({ text: await file.text(), name: file.name });
    });
    document.body.append(input);
    input.click();
  });
}

function download(name: string, text: string): void {
  const blob = new Blob([text], { type: "application/toml" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  a.click();
  URL.revokeObjectURL(url);
}
