// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import { render, screen } from "@testing-library/react";
import ErrorBoundary from "./ErrorBoundary";

const BuggyComponent = () => {
  return [][0]; // return undefined in a way that outsmarts Typescript.
};

const ThrowsLiteralComponent = () => {
  throw "simple string error"; // eslint-disable-line no-throw-literal
};

test("renders error", () => {
  render(
    <ErrorBoundary>
      <BuggyComponent />
    </ErrorBoundary>
  );
  const buggyComponentElement = screen.getByText(/BuggyComponent/);
  expect(buggyComponentElement).toBeInTheDocument();
  const sorryElement = screen.getByText(/Sorry/);
  expect(sorryElement).toBeInTheDocument();
});

test("renders string error", () => {
  render(
    <ErrorBoundary>
      <ThrowsLiteralComponent />
    </ErrorBoundary>
  );
  const msgElement = screen.getByText(/simple string error/);
  expect(msgElement).toBeInTheDocument();
});

test("renders child on success", () => {
  render(<ErrorBoundary>foo</ErrorBoundary>);
  const fooElement = screen.getByText(/foo/);
  expect(fooElement).toBeInTheDocument();
  const sorryElement = screen.queryByText(/Sorry/);
  expect(sorryElement).toBeNull();
});
