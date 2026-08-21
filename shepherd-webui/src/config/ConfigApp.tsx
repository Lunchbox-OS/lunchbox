/**
 * The config editor's shell.
 *
 * Used by both builds: as the whole app in the standalone bundle, and as one
 * lazily-loaded route inside the management UI. It takes no props and talks to
 * no daemon, which is what makes that work.
 */
import { useCallback, useState } from "react";
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
import DescriptionIcon from "@mui/icons-material/Description";
import FolderOpenIcon from "@mui/icons-material/FolderOpen";
import NoteAddIcon from "@mui/icons-material/NoteAdd";
import RedoIcon from "@mui/icons-material/Redo";
import SaveIcon from "@mui/icons-material/Save";
import UndoIcon from "@mui/icons-material/Undo";
import { ConfigDocProvider, useConfigDoc } from "./doc/ConfigDocProvider";
import type { Subject } from "./doc/patches";
import { focusFor, type FocusRequest } from "./navigation";
import { FileConfigSource, hasFileSystemAccess } from "./sources/FileConfigSource";
import { IssueList } from "./components/IssueList";
import { RawTomlPane } from "./components/RawTomlPane";
import { EntriesPage } from "./pages/EntriesPage";
import { GroupsPage } from "./pages/GroupsPage";
import { ServicePage } from "./pages/ServicePage";

type Page = "entries" | "groups" | "service" | "raw";

const fileSource = new FileConfigSource();

export function ConfigApp() {
  return (
    <ConfigDocProvider>
      <ConfigShell />
    </ConfigDocProvider>
  );
}

function ConfigShell() {
  const {
    ready,
    loadError,
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
          <Typography variant="h6" color="primary" sx={{ fontWeight: 700, mr: 2 }}>
            shepherd-launcher config editor
          </Typography>

          <Button size="small" startIcon={<FolderOpenIcon />} onClick={() => openFrom(fileSource)}>
            Open
          </Button>
          <Button size="small" startIcon={<NoteAddIcon />} onClick={startBlank}>
            New
          </Button>
          <Button
            size="small"
            variant="contained"
            startIcon={<SaveIcon />}
            onClick={() => save(fileSource)}
          >
            {fileSource.canSaveInPlace(doc) ? "Save" : "Download"}
          </Button>
          {fileSource.canSaveInPlace(doc) && (
            <Button size="small" onClick={() => save(fileSource, true)}>
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
        {!hasFileSystemAccess() && (
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
              onOpenGroup={(id) => focusOn({ kind: "group", id })}
            />
          )}
          {page === "groups" && (
            <GroupsPage
              config={view}
              focus={focusFor(focus, "group")}
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
