/**
 * The config editor's shell.
 *
 * Two homes, one component tree:
 *
 * - the **standalone** static bundle, where the source is the browsing
 *   computer's own files and there is no daemon behind anything;
 * - the **management UI**, where `src/App.tsx` mounts it as a full-screen
 *   takeover with a `DeviceConfigSource` and an `onClose` (issue #185).
 *
 * The difference is a prop, not a branch: everything below asks the source
 * what it can do rather than asking which build this is. `IS_STANDALONE`
 * survives for the one thing that is genuinely about the deployment — the
 * "Example" button, which in a device's own UI would read as an offer to
 * overwrite the device's config.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import AppBar from "@mui/material/AppBar";
import Alert from "@mui/material/Alert";
import Box from "@mui/material/Box";
import Button from "@mui/material/Button";
import Chip from "@mui/material/Chip";
import CircularProgress from "@mui/material/CircularProgress";
import Container from "@mui/material/Container";
import Divider from "@mui/material/Divider";
import IconButton from "@mui/material/IconButton";
import Snackbar from "@mui/material/Snackbar";
import Stack from "@mui/material/Stack";
import Tab from "@mui/material/Tab";
import Tabs from "@mui/material/Tabs";
import Toolbar from "@mui/material/Toolbar";
import Tooltip from "@mui/material/Tooltip";
import Typography from "@mui/material/Typography";
import ArrowBackIcon from "@mui/icons-material/ArrowBack";
import DescriptionIcon from "@mui/icons-material/Description";
import FolderOpenIcon from "@mui/icons-material/FolderOpen";
import RefreshIcon from "@mui/icons-material/Refresh";
import MenuBookIcon from "@mui/icons-material/MenuBook";
import NoteAddIcon from "@mui/icons-material/NoteAdd";
import RedoIcon from "@mui/icons-material/Redo";
import SaveIcon from "@mui/icons-material/Save";
import UndoIcon from "@mui/icons-material/Undo";
import { ConfigDocProvider, useConfigDoc } from "./doc/ConfigDocProvider";
import type { Subject } from "./doc/patches";
import { focusFor, type FocusRequest } from "./navigation";
import type { ConfigSource } from "./sources/ConfigSource";
import { ExampleConfigSource } from "./sources/ExampleConfigSource";
import { FileConfigSource, hasFileSystemAccess } from "./sources/FileConfigSource";
import { IS_STANDALONE } from "./target";
import { IssueList } from "./components/IssueList";
import { RawTomlPane } from "./components/RawTomlPane";
import { EntriesPage } from "./pages/EntriesPage";
import { GroupsPage } from "./pages/GroupsPage";
import { ServicePage } from "./pages/ServicePage";

type Page = "entries" | "groups" | "service" | "raw";

const fileSource = new FileConfigSource();
const exampleSource = new ExampleConfigSource();

export interface ConfigAppProps {
  /**
   * Where the document is read from and written back to. Defaults to the
   * browsing computer's files, which is what the standalone bundle wants and
   * what every existing test constructs.
   */
  source?: ConfigSource;
  /**
   * Open `source` as soon as the validator is ready, without waiting for
   * anyone to press anything. True for a device: arriving at "Config" and
   * being shown an empty document you have to go and fetch would be a puzzle,
   * not a choice.
   */
  autoOpen?: boolean;
  /**
   * Leave the editor. Present only where there is somewhere to go back to —
   * the management UI, which mounts this over its own navigation.
   */
  onClose?: () => void;
}

export function ConfigApp(props: ConfigAppProps = {}) {
  return (
    <ConfigDocProvider>
      <ConfigShell {...props} />
    </ConfigDocProvider>
  );
}

function ConfigShell({ source = fileSource, autoOpen = false, onClose }: ConfigAppProps) {
  const {
    ready,
    loadError,
    versions,
    view,
    report,
    document: doc,
    dirty,
    undo,
    redo,
    canUndo,
    canRedo,
    openFrom,
    save,
    startBlank,
    error,
    clearError,
  } = useConfigDoc();
  const [page, setPage] = useState<Page>("entries");

  // "Show me that activity/category" — from a category header on the board, or
  // from a validation issue. Switches to the page that owns the subject and
  // asks it to reveal it.
  const [focus, setFocus] = useState<FocusRequest | null>(null);
  const focusOn = useCallback((subject: Subject) => {
    setPage(subject.kind === "group" ? "groups" : "entries");
    setFocus((prev) => ({ subject, nonce: (prev?.nonce ?? 0) + 1 }));
  }, []);
  // A request is spent once the page has acted on it. See navigation.ts.
  const clearFocus = useCallback(() => setFocus(null), []);

  // Fetch the document once the validator is up. Guarded by a ref rather than
  // by the deps list: `openFrom` changes identity on every document change, so
  // a plain dependency would re-fetch the device's config after every save and
  // throw away whatever had been edited since.
  const opened = useRef(false);
  useEffect(() => {
    if (!autoOpen || !ready || opened.current) return;
    opened.current = true;
    void openFrom(source);
  }, [autoOpen, ready, openFrom, source]);

  // Re-read the device, losing anything unsaved. Only offered for a source
  // that can be re-read without a file picker, which is what `autoOpen`
  // already means.
  const reload = useCallback(() => {
    if (dirty && !window.confirm("Discard your unsaved changes and re-read the device?")) {
      return;
    }
    void openFrom(source);
  }, [dirty, openFrom, source]);

  // The one thing that is about the deployment rather than the source: this
  // browser's inability to overwrite a file it opened is a local-files
  // problem, and saying so beside a device config would be nonsense.
  const isLocalFiles = source instanceof FileConfigSource;

  // "Saved" is worth saying out loud for a device and not for a file: a file
  // save is visible in the title bar and in the operating system, while a
  // device save is a request that went somewhere the person cannot see, and
  // whose effect arrives a second later when the daemon re-reads the policy.
  const [saved, setSaved] = useState(false);
  const onSave = useCallback(async () => {
    if ((await save(source)) && !isLocalFiles) setSaved(true);
  }, [save, source, isLocalFiles]);

  if (loadError) {
    return (
      <Container sx={{ py: 4 }}>
        <Alert severity="error">
          The validator failed to load, so nothing here could check your file. {loadError}
        </Alert>
      </Container>
    );
  }

  if (!ready || !view) {
    return (
      <Stack sx={{ alignItems: "center", justifyContent: "center", minHeight: "60vh" }} spacing={2}>
        <CircularProgress />
        <Typography variant="body2" color="text.secondary">
          Loading the validator…
        </Typography>
      </Stack>
    );
  }

  const problems =
    report?.kind === "semantic"
      ? report.errors.length
      : report?.kind === "syntax" || report?.kind === "version"
        ? 1
        : 0;

  return (
    <Box sx={{ minHeight: "100dvh", display: "flex", flexDirection: "column" }}>
      <AppBar position="sticky" color="default" elevation={0} sx={{ borderBottom: "1px solid", borderColor: "divider" }}>
        <Toolbar sx={{ gap: 1, flexWrap: "wrap" }}>
          {onClose && (
            <Tooltip title="Back to the device">
              <IconButton size="small" onClick={onClose} aria-label="Back to the device" edge="start">
                <ArrowBackIcon />
              </IconButton>
            </Tooltip>
          )}
          <Typography variant="h6" color="primary" sx={{ fontWeight: 700 }}>
            {isLocalFiles ? "Lunchbox config editor" : "Configuration"}
          </Typography>
          {versions && (
            // Which build this is. The standalone editor is deployed on its own
            // subdomain and talks to no device, so nothing else on screen would
            // distinguish a stale cached bundle from a current one — and that is
            // the first thing worth knowing when it disagrees with a daemon.
            <Tooltip
              title={`Validates config_version ${versions.config_version}`}
              placement="bottom-start"
            >
              <Typography variant="caption" color="text.secondary" sx={{ mr: 2 }}>
                v{versions.crate_version}
              </Typography>
            </Tooltip>
          )}

          {isLocalFiles ? (
            <Button size="small" startIcon={<FolderOpenIcon />} onClick={() => openFrom(source)}>
              Open
            </Button>
          ) : (
            // A device has one config and it is already open, so the useful
            // verb is not "open another" but "show me what is actually on the
            // device now" — after an edit over SSH, or a save from another
            // browser.
            <Tooltip describeChild title="Re-read this device's config, discarding unsaved changes">
              <Button size="small" startIcon={<RefreshIcon />} onClick={reload}>
                Reload
              </Button>
            </Tooltip>
          )}
          {IS_STANDALONE && (
            // Standalone only. This build is a static page with nothing behind
            // it, so the example is the only thing it can open unprompted —
            // whereas in a device's own UI "Example" beside a real config would
            // read as an offer to overwrite it.
            // `describeChild` so the tooltip describes the button rather than
            // renaming it. Without it MUI puts the title in `aria-label`, and
            // the accessible name stops being the word on screen — the button
            // reads as "Open the annotated example…" to a screen reader, and
            // "click Example" matches nothing under voice control.
            <Tooltip
              describeChild
              title="Open the annotated example that ships with Lunchbox"
            >
              <Button
                size="small"
                startIcon={<MenuBookIcon />}
                onClick={() => openFrom(exampleSource)}
              >
                Example
              </Button>
            </Tooltip>
          )}
          {isLocalFiles && (
            // Deliberately not offered for a device: "New" on the file source
            // means "start a document"; beside a device's live config it would
            // read as an offer to blank it.
            <Button size="small" startIcon={<NoteAddIcon />} onClick={startBlank}>
              New
            </Button>
          )}
          <Button
            size="small"
            variant="contained"
            startIcon={<SaveIcon />}
            disabled={!isLocalFiles && !dirty}
            onClick={() => void onSave()}
          >
            {source.canSaveInPlace(doc) ? "Save" : "Download"}
          </Button>
          {isLocalFiles && source.canSaveInPlace(doc) && (
            <Button size="small" onClick={() => save(source, true)}>
              Save as…
            </Button>
          )}

          <Divider orientation="vertical" flexItem sx={{ mx: 1 }} />

          <Tooltip title="Undo">
            <span>
              <IconButton size="small" onClick={undo} disabled={!canUndo} aria-label="Undo">
                <UndoIcon fontSize="small" />
              </IconButton>
            </span>
          </Tooltip>
          <Tooltip title="Redo">
            <span>
              <IconButton size="small" onClick={redo} disabled={!canRedo} aria-label="Redo">
                <RedoIcon fontSize="small" />
              </IconButton>
            </span>
          </Tooltip>

          <Box sx={{ flex: 1 }} />

          <Stack direction="row" spacing={1} sx={{ alignItems: "center" }}>
            <DescriptionIcon fontSize="small" color="disabled" />
            <Typography variant="body2" color="text.secondary">
              {doc.name ?? "Untitled"}
              {dirty ? " •" : ""}
            </Typography>
            <Chip
              size="small"
              color={problems === 0 ? "success" : "error"}
              label={problems === 0 ? "Valid" : `${problems} problem${problems === 1 ? "" : "s"}`}
            />
          </Stack>
        </Toolbar>

        <Tabs value={page} onChange={(_, v) => setPage(v as Page)} sx={{ px: 2 }}>
          <Tab value="entries" label="Activities" />
          <Tab value="groups" label="Categories" />
          <Tab value="service" label="Device" />
          <Tab value="raw" label="TOML" />
        </Tabs>
      </AppBar>

      <Container maxWidth="xl" sx={{ py: 3, flex: 1, display: "flex", flexDirection: "column" }}>
        {isLocalFiles && !hasFileSystemAccess() && (
          <Alert severity="info" sx={{ mb: 2 }}>
            This browser cannot write files in place, so saving downloads a copy instead.
            Chromium-based browsers can save over the original.
          </Alert>
        )}

        {report && report.kind !== "semantic" && (
          <Box sx={{ mb: 2 }}>
            <IssueList report={report} />
          </Box>
        )}

        <Box sx={{ flex: 1, minHeight: 0 }}>
          {page === "entries" && (
            <EntriesPage
              config={view}
              focus={focusFor(focus, "entry")}
              onFocusHandled={clearFocus}
              onOpenGroup={(id) => focusOn({ kind: "group", id })}
            />
          )}
          {page === "groups" && (
            <GroupsPage
              config={view}
              focus={focusFor(focus, "group")}
              onFocusHandled={clearFocus}
              onOpenEntry={(id) => focusOn({ kind: "entry", id })}
            />
          )}
          {page === "service" && <ServicePage config={view} />}
          {page === "raw" && <RawTomlPane />}
        </Box>

        {page !== "raw" && report?.kind === "semantic" && report.errors.length > 0 && (
          <Box sx={{ mt: 3 }}>
            <Divider sx={{ mb: 2 }} />
            <IssueList
              report={report}
              onSelectEntry={(id) => focusOn({ kind: "entry", id })}
              onSelectGroup={(id) => focusOn({ kind: "group", id })}
            />
          </Box>
        )}
      </Container>

      <Snackbar
        open={saved}
        autoHideDuration={4000}
        onClose={() => setSaved(false)}
        anchorOrigin={{ vertical: "bottom", horizontal: "center" }}
      >
        <Alert severity="success" onClose={() => setSaved(false)}>
          Saved. The device picks the new policy up within a second.
        </Alert>
      </Snackbar>

      <Snackbar
        open={error !== null}
        autoHideDuration={6000}
        onClose={clearError}
        anchorOrigin={{ vertical: "bottom", horizontal: "center" }}
      >
        <Alert severity="error" onClose={clearError}>
          {error}
        </Alert>
      </Snackbar>
    </Box>
  );
}
