// @vitest-environment jsdom
//
// The wizard's directory list: rows can be edited, switched between
// read-only and read-write, removed, and added. The list itself is plain
// reducer data, submitted as `session_dirs`.

import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import { SessionDirsEditor } from "../SessionDirsEditor";
import type { SessionDirInput } from "../../../../lib/types";

const seeded: SessionDirInput[] = [{ path: "/home/u/commissura", access: "read-write" }];

function renderEditor(dirs: SessionDirInput[]) {
  const onChange = vi.fn();
  render(<SessionDirsEditor dirs={dirs} onChange={onChange} />);
  return { onChange };
}

afterEach(cleanup);

describe("SessionDirsEditor", () => {
  it("shows the pre-filled entries", () => {
    renderEditor(seeded);
    expect((screen.getByLabelText("Directory 1 path") as HTMLInputElement).value).toBe("/home/u/commissura");
    expect((screen.getByLabelText("Directory 1 access") as HTMLSelectElement).value).toBe("read-write");
  });

  it("says only the working directory is reachable when the list is empty", () => {
    renderEditor([]);
    expect(screen.getByText("Only the working directory.")).toBeTruthy();
  });

  it("changes a row's access", () => {
    const { onChange } = renderEditor(seeded);
    fireEvent.change(screen.getByLabelText("Directory 1 access"), { target: { value: "read-only" } });
    expect(onChange).toHaveBeenCalledWith([{ path: "/home/u/commissura", access: "read-only" }]);
  });

  it("edits a row's path", () => {
    const { onChange } = renderEditor(seeded);
    fireEvent.change(screen.getByLabelText("Directory 1 path"), { target: { value: "/srv/ref" } });
    expect(onChange).toHaveBeenCalledWith([{ path: "/srv/ref", access: "read-write" }]);
  });

  it("removes a row, including a pre-filled one", () => {
    const { onChange } = renderEditor(seeded);
    fireEvent.click(screen.getByLabelText("Remove directory 1"));
    expect(onChange).toHaveBeenCalledWith([]);
  });

  it("adds an empty read-only row", () => {
    const { onChange } = renderEditor(seeded);
    fireEvent.click(screen.getByText("+ Add directory"));
    expect(onChange).toHaveBeenCalledWith([...seeded, { path: "", access: "read-only" }]);
  });
});
