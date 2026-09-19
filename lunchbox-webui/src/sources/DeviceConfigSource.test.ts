/**
 * The device as a `ConfigSource` (issue #185).
 *
 * The two things worth pinning down are both about the `ETag`, because both
 * are silent when they go wrong: a save that forgets to send the tag back
 * would clobber whatever `sudoedit` did in the meantime, and a 412 shown as a
 * bare "Request failed with status code 412" would leave a parent with no idea
 * that their edit is still recoverable.
 */
import { afterEach, describe, expect, it, vi } from "vitest";

const getDeviceConfig = vi.fn();
const putDeviceConfig = vi.fn();

class FakeApiError extends Error {
  constructor(
    public status: number,
    public code: string,
    message: string,
  ) {
    super(message);
  }
}

vi.mock("../api/client", () => ({
  ApiError: FakeApiError,
  getDeviceConfig: (...a: unknown[]) => getDeviceConfig(...a),
  putDeviceConfig: (...a: unknown[]) => putDeviceConfig(...a),
}));

const { DeviceConfigSource } = await import("./DeviceConfigSource");

afterEach(() => vi.clearAllMocks());

describe("editing the device's own config", () => {
  it("carries the ETag from the read into the write", async () => {
    getDeviceConfig.mockResolvedValue({ text: "config_version = 1\n", etag: '"abc"' });
    putDeviceConfig.mockResolvedValue({ text: "next", etag: '"def"' });
    const source = new DeviceConfigSource();

    const opened = await source.open();
    expect(opened?.text).toBe("config_version = 1\n");

    const saved = await source.save(opened!, "next");
    expect(putDeviceConfig).toHaveBeenCalledWith("next", '"abc"');
    // And the new tag replaces the old one, so a second save in the same
    // sitting is not a conflict with itself.
    expect(saved.handle).toBe('"def"');
  });

  it("sends no tag when it has none, which the daemon reads as overwrite", async () => {
    putDeviceConfig.mockResolvedValue({ text: "x", etag: '"1"' });
    await new DeviceConfigSource().save({ text: "", name: null }, "x");
    expect(putDeviceConfig).toHaveBeenCalledWith("x", null);
  });

  it("explains a 412 as somebody else having edited the file", async () => {
    putDeviceConfig.mockRejectedValue(
      new FakeApiError(412, "precondition_failed", "The policy on the device changed"),
    );
    await expect(
      new DeviceConfigSource().save({ text: "", name: null, handle: '"old"' }, "x"),
    ).rejects.toThrow(/changed while this was open/);
  });

  it("passes the daemon's own words through when it rejects a config", async () => {
    // The wasm validator said yes and the daemon said no, so the two disagree
    // and paraphrasing would hide which.
    putDeviceConfig.mockRejectedValue(
      new FakeApiError(422, "unprocessable", "entry 'a': unknown kind"),
    );
    await expect(
      new DeviceConfigSource().save({ text: "", name: null, handle: '"t"' }, "x"),
    ).rejects.toThrow(/unknown kind/);
  });

  it("always saves in place: a device has one config and one place for it", () => {
    expect(new DeviceConfigSource().canSaveInPlace()).toBe(true);
  });
});
