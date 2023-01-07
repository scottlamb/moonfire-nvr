// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

/**
 * @fileoverview Main application
 *
 * This defines `<Frame>` to lay out the visual structure of the application:
 *
 * - top menu bar with fixed components and a spot for activities to add
 *   their own elements
 * - navigation drawer
 * - main activity error
 *
 * It handles the login state and, once logged in, delegates to the appropriate
 * activity based on the URL. Each activity is expected to return the supplied
 * `<Frame>` with its own `children` and optionally `activityMenuPart` filled
 * in.
 */

import Container from "@mui/material/Container";
import React, { useEffect, useReducer, useState } from "react";
import * as api from "./api";
import MoonfireMenu from "./AppMenu";
import Login from "./Login";
import { useSnackbars } from "./snackbars";
import ListActivity from "./List";
import AppBar from "@mui/material/AppBar";
import { Routes, Route, Link, Navigate } from "react-router-dom";
import LiveActivity from "./Live";
import UsersActivity from "./Users";
import Drawer from "@mui/material/Drawer";
import List from "@mui/material/List";
import ListItem from "@mui/material/ListItem";
import ListItemText from "@mui/material/ListItemText";
import ListIcon from "@mui/icons-material/List";
import PeopleIcon from "@mui/icons-material/People";
import Videocam from "@mui/icons-material/Videocam";
import ListItemIcon from "@mui/material/ListItemIcon";
import ChangePassword from "./ChangePassword";

export type LoginState =
  | "unknown"
  | "logged-in"
  | "not-logged-in"
  | "server-requires-login"
  | "user-requested-login";

export interface FrameProps {
  activityMenuPart?: JSX.Element;
  children?: React.ReactNode;
}

function App() {
  const [showMenu, toggleShowMenu] = useReducer((m: boolean) => !m, false);
  const [toplevel, setToplevel] = useState<api.ToplevelResponse | null>(null);
  const [timeZoneName, setTimeZoneName] = useState<string | null>(null);
  const [fetchSeq, setFetchSeq] = useState(0);
  const [loginState, setLoginState] = useState<LoginState>("unknown");
  const [changePasswordOpen, setChangePasswordOpen] = useState<boolean>(false);
  const [error, setError] = useState<api.FetchError | null>(null);
  const needNewFetch = () => setFetchSeq((seq) => seq + 1);
  const snackbars = useSnackbars();

  const onLoginSuccess = () => {
    setLoginState("logged-in");
    needNewFetch();
  };

  const logout = async () => {
    const resp = await api.logout(
      {
        csrf: toplevel!.user!.session!.csrf,
      },
      {}
    );
    switch (resp.status) {
      case "aborted":
        break;
      case "error":
        snackbars.enqueue({
          message: "Logout failed: " + resp.message,
        });
        break;
      case "success":
        needNewFetch();
        break;
    }
  };

  useEffect(() => {
    const abort = new AbortController();
    const doFetch = async (signal: AbortSignal) => {
      const resp = await api.toplevel({ signal });
      switch (resp.status) {
        case "aborted":
          break;
        case "error":
          if (resp.httpStatus === 401) {
            setLoginState("server-requires-login");
            return;
          }
          setError(resp);
          break;
        case "success":
          setError(null);
          setLoginState(
            resp.response.user?.session === undefined
              ? "not-logged-in"
              : "logged-in"
          );
          setToplevel(resp.response);
          setTimeZoneName(resp.response.timeZoneName);
      }
    };
    doFetch(abort.signal);
    return () => {
      abort.abort();
    };
  }, [fetchSeq]);

  const Frame = ({ activityMenuPart, children }: FrameProps): JSX.Element => {
    return (
      <>
        <AppBar position="static">
          <MoonfireMenu
            loginState={loginState}
            requestLogin={() => {
              setLoginState("user-requested-login");
            }}
            logout={logout}
            changePassword={() => setChangePasswordOpen(true)}
            menuClick={toggleShowMenu}
            activityMenuPart={activityMenuPart}
          />
        </AppBar>
        <Drawer
          variant="temporary"
          open={showMenu}
          onClose={toggleShowMenu}
          ModalProps={{
            keepMounted: true,
          }}
        >
          <List>
            <ListItem
              button
              key="list"
              onClick={toggleShowMenu}
              component={Link}
              to="/"
            >
              <ListItemIcon>
                <ListIcon />
              </ListItemIcon>
              <ListItemText primary="List view" />
            </ListItem>
            <ListItem
              button
              key="live"
              onClick={toggleShowMenu}
              component={Link}
              to="/live"
            >
              <ListItemIcon>
                <Videocam />
              </ListItemIcon>
              <ListItemText primary="Live view (experimental)" />
            </ListItem>
            {toplevel?.permissions.adminUsers && (
              <ListItem
                button
                key="users"
                onClick={toggleShowMenu}
                component={Link}
                to="/users"
              >
                <ListItemIcon>
                  <PeopleIcon />
                </ListItemIcon>
                <ListItemText primary="Users" />
              </ListItem>
            )}
          </List>
        </Drawer>
        <Login
          onSuccess={onLoginSuccess}
          open={
            loginState === "server-requires-login" ||
            loginState === "user-requested-login"
          }
          handleClose={() => {
            setLoginState((s) =>
              s === "user-requested-login" ? "not-logged-in" : s
            );
          }}
        />
        {toplevel?.user !== undefined && (
          <ChangePassword
            open={changePasswordOpen}
            user={toplevel?.user}
            handleClose={() => setChangePasswordOpen(false)}
          />
        )}
        {error !== null && (
          <Container>
            <h2>Error querying server</h2>
            <pre>{error.message}</pre>
            <p>
              You may find more information in the Javascript console. Try
              reloading the page once you believe the problem is resolved.
            </p>
          </Container>
        )}
        {children}
      </>
    );
  };

  if (toplevel == null) {
    return <Frame />;
  }
  return (
    <Routes>
      <Route
        path=""
        element={
          <ListActivity
            toplevel={toplevel}
            timeZoneName={timeZoneName!}
            Frame={Frame}
          />
        }
      />
      <Route
        path="live"
        element={<LiveActivity cameras={toplevel.cameras} Frame={Frame} />}
      />
      <Route
        path="users"
        element={
          <UsersActivity Frame={Frame} csrf={toplevel!.user?.session?.csrf} />
        }
      />
      <Route path="*" element={<Navigate to="/" replace />} />
    </Routes>
  );
}

export default App;
