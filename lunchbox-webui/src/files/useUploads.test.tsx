// @vitest-environment jsdom
/**
 * The upload queue on a bad link (issue #195).
 *
 * These devices are often repurposed hardware with the wifi chip they came
 * with, so an interrupted transfer is the expected case rather than the
 * unlucky one. What is worth asserting is the behaviour that only shows up
 * when something goes wrong: that a big file goes up in pieces, that a
 * resumed one asks the device where it got to, that a transient failure is
 * retried and a decision is not, and that a stalled request is cut off rather
 * than waited on.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

const uploadFile = vi.fn();
const uploadChunk = vi.fn();
const uploadOffset = vi.fn();
const abandonUpload = vi.fn();

vi.mock("../api/files", async () => {
  const actual =
    await vi.importActual<typeof import("../api/files")>("../api/files");
  return {
    ...actual,
    // Small enough to make a 20-byte file "large" without allocating anything.
    CHUNK_BYTES: 8,
    uploadFile: (...args: unknown[]) => uploadFile(...args),
    uploadChunk: (...args: unknown[]) => uploadChunk(...args),
    uploadOffset: (...args: unknown[]) => uploadOffset(...args),
    abandonUpload: (...args: unknown[]) => abandonUpload(...args),
  };
});

const { UploadsProvider, useUploads, isTransient } = await import("./useUploads");
const { TransferTray } = await import("./TransferTray");
const { ApiError } = await import("../api/client");

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  vi.useRealTimers();
});

beforeEach(() => {
  uploadOffset.mockResolvedValue(0);
  abandonUpload.mockResolvedValue(undefined);
});

/** A harness that exposes `start` and renders the tray. */
function Harness({ file }: { file: File }) {
  const uploads = useUploads();
  return (
    <>
      <button
        type="button"
        onClick={() =>
          uploads.start({ rootId: "home", dir: "Books", files: [file] })
        }
      >
        go
      </button>
      <TransferTray />
    </>
  );
}

async function startUpload(file: File) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={client}>
      <UploadsProvider>
        <Harness file={file} />
      </UploadsProvider>
    </QueryClientProvider>,
  );
  await userEvent.click(screen.getByText("go"));
}

function bigFile(bytes = 20): File {
  return new File(["x".repeat(bytes)], "video.mp4");
}

describe("uploading on a link that drops", () => {
  it("sends anything past one chunk in pieces", async () => {
    uploadChunk.mockResolvedValue(null);
    await startUpload(bigFile(20));

    // 20 bytes at a chunk size of 8: 0-7, 8-15, 16-19.
    await waitFor(() => expect(uploadChunk).toHaveBeenCalledTimes(3));
    const offsets = uploadChunk.mock.calls.map((c) => [c[4], c[5]]);
    expect(offsets).toEqual([
      [0, 20],
      [8, 20],
      [16, 20],
    ]);
  });

  it("asks the device where it got to, and carries on from there", async () => {
    // A previous attempt left 8 bytes on the device.
    uploadOffset.mockResolvedValue(8);
    uploadChunk.mockResolvedValue(null);
    await startUpload(bigFile(20));

    await waitFor(() => expect(uploadChunk).toHaveBeenCalledTimes(2));
    expect(uploadChunk.mock.calls.map((c) => c[4])).toEqual([8, 16]);
  });

  it("says it resumed, rather than letting it look like a restart", async () => {
    uploadOffset.mockResolvedValue(8);
    // Held open, so the tray is caught mid-transfer rather than after it.
    uploadChunk.mockImplementation(() => new Promise(() => {}));
    await startUpload(bigFile(20));

    // "It started again from zero" is what a person watching a slow link is
    // afraid of, so the tray says when it did not.
    expect(await screen.findByText(/resumed from/)).toBeTruthy();
  });

  it("takes the device's word when the two disagree about the offset", async () => {
    uploadChunk
      .mockRejectedValueOnce(
        new ApiError(409, "conflict", "this device has 8 bytes of that upload, not 0"),
      )
      .mockResolvedValue(null);
    uploadOffset.mockResolvedValueOnce(0).mockResolvedValue(8);
    await startUpload(bigFile(20));

    // Not a retry of the same bytes: it re-syncs and continues from 8.
    await waitFor(() => expect(uploadChunk).toHaveBeenCalledTimes(3), {
      timeout: 5_000,
    });
    expect(uploadChunk.mock.calls.map((c) => c[4])).toEqual([0, 8, 16]);
  });

  it("retries a dropped connection from where the device got to", async () => {
    uploadChunk
      .mockRejectedValueOnce(new Error("Network Error"))
      .mockResolvedValue(null);
    // The drop landed mid-chunk, so the device holds 4 of the first 8 bytes.
    uploadOffset.mockResolvedValueOnce(0).mockResolvedValue(4);
    await startUpload(bigFile(20));

    await waitFor(() => expect(uploadChunk).toHaveBeenCalledTimes(3), {
      timeout: 4_000,
    });
    // Not from the chunk boundary — from byte 4. Re-sending the four bytes the
    // device already had is exactly the waste a bad link cannot afford, and it
    // also means the run finishes in three requests rather than four.
    expect(uploadChunk.mock.calls.map((c) => c[4])).toEqual([0, 4, 12]);
  });

  it("does not re-send gigabytes to hear the same refusal twice", async () => {
    // A decision, not a mishap: the device will say the same thing next time.
    uploadChunk.mockRejectedValue(new ApiError(413, "too_large", "too big"));
    await startUpload(bigFile(20));

    await waitFor(() => expect(screen.getByText(/too big/)).toBeTruthy());
    expect(uploadChunk).toHaveBeenCalledTimes(1);
  });

  it("offers a retry that resumes rather than restarts", async () => {
    // A refusal rather than a drop, so the transfer reaches its error state
    // without spending the backoff this test is not about.
    uploadChunk.mockRejectedValue(new ApiError(403, "forbidden", "no"));
    await startUpload(bigFile(20));
    const retry = await screen.findByRole("button", { name: "Retry" });

    // The device kept what it had; the retry picks it up.
    uploadOffset.mockResolvedValue(16);
    uploadChunk.mockReset();
    uploadChunk.mockResolvedValue(null);
    await userEvent.click(retry);

    await waitFor(() => expect(uploadChunk).toHaveBeenCalledTimes(1));
    expect(uploadChunk.mock.calls[0][4]).toBe(16);
  });

  it("gives up on a request that stops moving", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    // A connection that stalls without closing is the common wifi failure, and
    // nothing below the application notices it quickly.
    let aborted = false;
    uploadChunk.mockImplementation(
      (...args: unknown[]) =>
        new Promise((_resolve, reject) => {
          const options = args[7] as { signal?: AbortSignal };
          options.signal?.addEventListener("abort", () => {
            aborted = true;
            reject(new Error("canceled"));
          });
        }),
    );
    await startUpload(bigFile(20));
    await waitFor(() => expect(uploadChunk).toHaveBeenCalled());

    await act(async () => {
      await vi.advanceTimersByTimeAsync(40_000);
    });
    expect(aborted).toBe(true);
  });

  it("throws away what the device is holding when a transfer is cancelled", async () => {
    uploadChunk.mockImplementation(() => new Promise(() => {}));
    await startUpload(bigFile(20));
    await waitFor(() => expect(uploadChunk).toHaveBeenCalled());

    await userEvent.click(screen.getByLabelText("Cancel video.mp4"));
    // A cancel that left gigabytes on a small disk until tomorrow is not a
    // cancel.
    await waitFor(() =>
      expect(abandonUpload).toHaveBeenCalledWith("home", "Books/video.mp4", expect.any(String)),
    );
  });

  it("sends a small file in one request, as before", async () => {
    uploadFile.mockResolvedValue({ path: "Books/note.txt", size: 4, etag: "4-1" });
    await startUpload(new File(["abcd"], "note.txt"));

    await waitFor(() => expect(uploadFile).toHaveBeenCalledTimes(1));
    expect(uploadChunk).not.toHaveBeenCalled();
  });
});

describe("which failures are worth retrying", () => {
  it("retries what might work, and accepts what will not", () => {
    expect(isTransient(new Error("Network Error"))).toBe(true);
    expect(isTransient(new ApiError(500, "internal", "boom"))).toBe(true);
    expect(isTransient(new ApiError(413, "too_large", "no"))).toBe(false);
    expect(isTransient(new ApiError(507, "insufficient_storage", "no"))).toBe(false);
    expect(isTransient(new ApiError(412, "precondition_failed", "no"))).toBe(false);
    expect(isTransient(new ApiError(403, "forbidden", "no"))).toBe(false);
  });
});
