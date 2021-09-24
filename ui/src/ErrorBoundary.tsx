// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import Avatar from "@mui/material/Avatar";
import Container from "@mui/material/Container";
import BugReportIcon from "@mui/icons-material/BugReport";
import React from "react";

interface State {
  error: any;
}

interface Props {
  children: React.ReactNode;
}

/**
 * A simple <a href="https://reactjs.org/docs/error-boundaries.html">error
 * boundary</a> meant to go at the top level.
 *
 * The assumption is that any error here is a bug in the UI layer. Components
 * shouldn't throw errors upward even if there are network or server problems.
 *
 * Limitations: as described in the React docs, error boundaries don't catch
 * errors in async code / rejected Promises.
 */
class MoonfireErrorBoundary extends React.Component<Props, State> {
  constructor(props: Props) {
    super(props);
    this.state = { error: null };
  }

  static getDerivedStateFromError(error: any) {
    return { error };
  }

  componentDidCatch(error: any, errorInfo: React.ErrorInfo) {
    console.error("Uncaught error:", error, errorInfo);
  }

  render() {
    const { children } = this.props;

    if (this.state.error !== null) {
      var error;
      if (this.state.error.stack !== undefined) {
        error = <pre>{this.state.error.stack}</pre>;
      } else if (this.state.error instanceof Error) {
        error = (
          <>
            <pre>{this.state.error.name}</pre>
            <pre>{this.state.error.message}</pre>
          </>
        );
      } else {
        error = <pre>{this.state.error}</pre>;
      }

      return (
        <Container>
          <Avatar
            sx={{
              float: "left",
              bgcolor: "secondary.main",
              marginRight: "1em",
            }}
          >
            <BugReportIcon color="primary" />
          </Avatar>
          <h1>Error</h1>

          <p>
            Sorry! You've found a bug in Moonfire NVR. We need a good bug report
            to get it fixed. Can you help?
          </p>

          <h2>How to report a bug</h2>

          <p>
            Please open{" "}
            <a href="https://github.com/scottlamb/moonfire-nvr/issues">
              Moonfire NVR's issue tracker
            </a>{" "}
            and see if this problem has already been reported.
          </p>

          <h3>Can't find anything?</h3>

          <p>Open a new issue with as much detail as you can:</p>

          <ul>
            <li>the version of Moonfire NVR you're using</li>
            <li>
              your environment, including:
              <ul>
                <li>web browser: Chrome, Firefox, Safari, etc.</li>
                <li>platform: macOS, Windows, Linux, Android, iOS, etc.</li>
                <li>browser extensions</li>
                <li>anything special about your Moonfire NVR setup</li>
              </ul>
            </li>
            <li>all the errors you see in your browser's Javascript console</li>
            <li>steps to reproduce, if possible</li>
          </ul>

          <h3>Already reported?</h3>

          <ul>
            <li>+1 the issue so we know more people are affected.</li>
            <li>add any new details you've noticed.</li>
          </ul>

          <h2>The error</h2>

          {error}
        </Container>
      );
    }
    return children;
  }
}

export default MoonfireErrorBoundary;
