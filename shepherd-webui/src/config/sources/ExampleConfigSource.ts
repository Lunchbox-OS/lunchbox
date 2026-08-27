/**
 * The annotated example config that ships with shepherd, for trying the editor
 * out.
 *
 * The standalone editor is a static page with no device behind it, so the
 * first thing it can show anyone is an empty document — which demonstrates
 * nothing. `config.example.toml` exercises most of the schema and is heavily
 * commented, so opening it is both a demo and the fastest way to see that the
 * editor really does preserve comments.
 *
 * The file is inlined at build time rather than fetched, so this works from a
 * static host, offline, and over `file://` in a future desktop shell — and
 * cannot fail at the moment someone clicks the button. It is the same file the
 * repo ships and CI validates, imported directly rather than copied, so the
 * demo cannot drift from the schema the daemon accepts.
 *
 * It costs ~41 kB, and only in the standalone bundle: this whole tree is
 * absent from the build `rust-embed` compiles into shepherdd. Worth a second
 * look if the editor is ever routed into the management UI — see the note in
 * `src/App.tsx` about what else rides along.
 */
import exampleToml from "../../../../config.example.toml?raw";
import type { ConfigDocument, ConfigSource } from "./ConfigSource";
import { FileConfigSource } from "./FileConfigSource";

/** What the title bar shows, and what a download of it is named. */
const EXAMPLE_NAME = "config.example.toml";

export class ExampleConfigSource implements ConfigSource {
  readonly label = "Example configuration";

  /**
   * There is nowhere to save an inlined asset back to, so saving an edited
   * example is saving a file like any other — including the download fallback
   * on browsers without the File System Access API.
   */
  private readonly files = new FileConfigSource();

  canSaveInPlace(): boolean {
    return false;
  }

  async open(): Promise<ConfigDocument> {
    return { text: exampleToml, name: EXAMPLE_NAME };
  }

  save(doc: ConfigDocument, text: string): Promise<ConfigDocument> {
    return this.files.save(doc, text);
  }

  saveAs(doc: ConfigDocument, text: string): Promise<ConfigDocument | null> {
    return this.files.saveAs(doc, text);
  }
}
