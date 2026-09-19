/** The two questions a write has to ask before it happens (issue #195). */
import { useEffect, useState } from "react";
import Alert from "@mui/material/Alert";
import Button from "@mui/material/Button";
import Dialog from "@mui/material/Dialog";
import DialogActions from "@mui/material/DialogActions";
import DialogContent from "@mui/material/DialogContent";
import DialogContentText from "@mui/material/DialogContentText";
import DialogTitle from "@mui/material/DialogTitle";
import TextField from "@mui/material/TextField";

/** Characters a name cannot contain, checked before the device says so. */
const ILLEGAL = /[/\u0000]/;

export function NewFolderDialog({
  open,
  where,
  busy,
  error,
  onCancel,
  onCreate,
}: {
  open: boolean;
  /** Where it will be created, for the sentence. */
  where: string;
  busy: boolean;
  error: string | null;
  onCancel: () => void;
  onCreate: (name: string) => void;
}) {
  const [name, setName] = useState("");
  useEffect(() => {
    if (open) setName("");
  }, [open]);

  const illegal = ILLEGAL.test(name);
  const ready = name.trim().length > 0 && !illegal && !busy;

  return (
    <Dialog open={open} onClose={onCancel} fullWidth maxWidth="xs">
      <form
        onSubmit={(e) => {
          e.preventDefault();
          if (ready) onCreate(name.trim());
        }}
      >
        <DialogTitle>New folder</DialogTitle>
        <DialogContent>
          <DialogContentText variant="body2" sx={{ mb: 2 }}>
            Inside {where}.
          </DialogContentText>
          {error && (
            <Alert severity="error" sx={{ mb: 2 }}>
              {error}
            </Alert>
          )}
          <TextField
            autoFocus
            fullWidth
            size="small"
            label="Name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            error={illegal}
            helperText={illegal ? "A name cannot contain a slash." : " "}
          />
        </DialogContent>
        <DialogActions>
          <Button onClick={onCancel}>Cancel</Button>
          <Button type="submit" variant="contained" disabled={!ready}>
            Create
          </Button>
        </DialogActions>
      </form>
    </Dialog>
  );
}

export function DeleteConfirmDialog({
  open,
  name,
  isFolder,
  busy,
  error,
  onCancel,
  onDelete,
}: {
  open: boolean;
  name: string;
  isFolder: boolean;
  busy: boolean;
  error: string | null;
  onCancel: () => void;
  onDelete: () => void;
}) {
  return (
    <Dialog open={open} onClose={onCancel} fullWidth maxWidth="xs">
      <DialogTitle>Delete {name}?</DialogTitle>
      <DialogContent>
        {error && (
          <Alert severity="error" sx={{ mb: 2 }}>
            {error}
          </Alert>
        )}
        <DialogContentText variant="body2">
          {isFolder
            ? // Said plainly, because the recursive flag is the difference
              // between one row going and a shelf of books going.
              "This deletes the folder and everything in it. It cannot be undone from here."
            : "This cannot be undone from here."}
        </DialogContentText>
      </DialogContent>
      <DialogActions>
        <Button onClick={onCancel}>Cancel</Button>
        <Button color="error" variant="contained" disabled={busy} onClick={onDelete}>
          {busy ? "Deleting…" : "Delete"}
        </Button>
      </DialogActions>
    </Dialog>
  );
}
