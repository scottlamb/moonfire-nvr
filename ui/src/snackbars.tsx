// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

/**
 * App-wide provider for imperative snackbar.
 *
 * I chose not to use the popular
 * <a href="https://www.npmjs.com/package/notistack">notistack</a> because it
 * doesn't seem oriented for complying with the <a
 * href="https://material.io/components/snackbars">material.io spec</a>.
 * Besides supporting non-compliant behaviors (eg <tt>maxSnack</tt> > 1</tt>),
 * it doesn't actually enqueue notifications. Newer ones replace older ones.
 *
 * This isn't as flexible as <tt>notistack</tt> because I don't need that
 * flexibility (yet).
 */

import IconButton from "@mui/material/IconButton";
import Snackbar, {
  SnackbarCloseReason,
  SnackbarProps,
} from "@mui/material/Snackbar";
import CloseIcon from "@mui/icons-material/Close";
import React, { useContext } from "react";

interface SnackbarProviderProps {
  /**
   * The autohide duration to use if none is provided to <tt>enqueue</tt>.
   */
  autoHideDuration: number;

  children: React.ReactNode;
}

export interface MySnackbarProps extends Omit<
  SnackbarProps,
  | "key"
  | "anchorOrigin"
  | "open"
  | "handleClosed"
  | "TransitionProps"
  | "actions"
> {
  key?: React.Key;
}

type MySnackbarPropsWithRequiredKey = Omit<MySnackbarProps, "key"> &
  Required<Pick<MySnackbarProps, "key">>;
interface Enqueued extends MySnackbarPropsWithRequiredKey {
  open: boolean;
}

/**
 * Imperative interface to enqueue and close app-wide snackbars.
 * These methods should be called from effects (not directly from render).
 */
export interface Snackbars {
  /**
   * Enqueues a snackbar.
   *
   * @param snackbar
   * The snackbar to add. The only required property is <tt>message</tt>. If
   * <tt>key</tt> is present, it will close any message with the same key
   * immediately, as well as be returned so it can be passed to close again
   * later. Note that currently several properties are used internally and
   * can't be specified, including <tt>actions</tt>.
   * @return A key that can be passed to close: the caller-specified key if
   * possible, or an internally generated key otherwise.
   */
  enqueue: (snackbar: MySnackbarProps) => React.Key;

  /**
   * Closes a snackbar if present.
   *
   * If it is currently visible, it will be allowed to gracefully close.
   * Otherwise it's removed from the queue.
   */
  close: (key: React.Key) => void;
}

interface State {
  queue: Enqueued[];
}

const ctx = React.createContext<Snackbars | null>(null);

/**
 * Provides a <tt>Snackbars</tt> instance for use by <tt>useSnackbars</tt>.
 */
// This is a class because I want to guarantee the context value never changes,
// and I couldn't figure out a way to do that with hooks.
export class SnackbarProvider
  extends React.Component<SnackbarProviderProps, State>
  implements Snackbars
{
  constructor(props: SnackbarProviderProps) {
    super(props);
    this.state = { queue: [] };
  }

  autoKeySeq = 0;

  enqueue(snackbar: MySnackbarProps): React.Key {
    const key =
      snackbar.key === undefined ? `auto-${this.autoKeySeq++}` : snackbar.key;
    // TODO: filter existing.
    this.setState((state) => ({
      queue: [...state.queue, { key, open: true, ...snackbar }],
    }));
    return key;
  }

  handleCloseSnackbar = (
    key: React.Key,
    event: Event | React.SyntheticEvent<any>,
    reason: SnackbarCloseReason,
  ) => {
    if (reason === "clickaway") return;
    this.setState((state) => {
      const snack = state.queue[0];
      if (snack?.key !== key) {
        console.warn(`Active snack is ${snack?.key}; expected ${key}`);
        return null; // no change.
      }
      const newSnack: Enqueued = { ...snack, open: false };
      return { queue: [newSnack, ...state.queue.slice(1)] };
    });
  };

  handleSnackbarExited = (key: React.Key) => {
    this.setState((state) => ({ queue: state.queue.slice(1) }));
  };

  close(key: React.Key): void {
    this.setState((state) => {
      // If this is the active snackbar, let it close gracefully, as in
      // handleCloseSnackbar.
      if (state.queue[0]?.key === key) {
        const newSnack: Enqueued = { ...state.queue[0], open: false };
        return { queue: [newSnack, ...state.queue.slice(1)] };
      }
      // Otherwise, remove it before it shows up at all.
      return { queue: state.queue.filter((e: Enqueued) => e.key !== key) };
    });
  }

  render(): JSX.Element {
    const first = this.state.queue[0];
    const snackbars: Snackbars = this;
    return (
      <ctx.Provider value={snackbars}>
        {this.props.children}
        {first === undefined ? null : (
          <Snackbar
            {...first}
            anchorOrigin={{
              vertical: "bottom",
              horizontal: "left",
            }}
            autoHideDuration={
              first.autoHideDuration ?? this.props.autoHideDuration
            }
            onClose={(event, reason) =>
              this.handleCloseSnackbar(first.key, event, reason)
            }
            TransitionProps={{
              onExited: () => this.handleSnackbarExited(first.key),
            }}
            action={
              <IconButton
                size="small"
                aria-label="close"
                color="inherit"
                onClick={() => this.close(first.key)}
              >
                <CloseIcon fontSize="small" />
              </IconButton>
            }
          />
        )}
      </ctx.Provider>
    );
  }
}

/** Returns a <tt>Snackbars</tt> from context. */
export function useSnackbars(): Snackbars {
  return useContext(ctx)!;
}
