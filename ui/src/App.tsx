// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import Container from "@mui/material/Container";
import React, { useEffect, useReducer, useState } from "react";
import * as api from "./api";
import MoonfireMenu from "./AppMenu";
import Login from "./Login";
import { useSnackbars } from "./snackbars";
import ListActivity from "./List";
import AppBar from "@mui/material/AppBar";
import {
  Routes,
  Route,
  Link,
  useSearchParams,
  useResolvedPath,
  useMatch,
} from "react-router-dom";
import LiveActivity, { MultiviewChooser } from "./Live";
import Drawer from "@mui/material/Drawer";
import List from "@mui/material/List";
import ListItem from "@mui/material/ListItem";
import ListItemText from "@mui/material/ListItemText";
import ListIcon from "@mui/icons-material/List";
import Videocam from "@mui/icons-material/Videocam";
import ListItemIcon from "@mui/material/ListItemIcon";
import FilterList from "@mui/icons-material/FilterList";
import IconButton from "@mui/material/IconButton";

export type LoginState =
  | "unknown"
  | "logged-in"
  | "not-logged-in"
  | "server-requires-login"
  | "user-requested-login";

type Activity = "list" | "live";

function App() {
  const [showMenu, toggleShowMenu] = useReducer((m: boolean) => !m, false);
  const [searchParams, setSearchParams] = useSearchParams();

  const [showListSelectors, toggleShowListSelectors] = useReducer(
    (m: boolean) => !m,
    true
  );
  let resolved = useResolvedPath("live");
  let match = useMatch({ path: resolved.pathname, end: true });
  const [activity, setActivity] = useState<Activity>(match ? "live" : "list");
  const [multiviewLayoutIndex, setMultiviewLayoutIndex] = useState(
    Number.parseInt(searchParams.get("layout") || "0", 10)
  );
  const [toplevel, setToplevel] = useState<api.ToplevelResponse | null>(null);
  const [timeZoneName, setTimeZoneName] = useState<string | null>(null);
  const [fetchSeq, setFetchSeq] = useState(0);
  const [loginState, setLoginState] = useState<LoginState>("unknown");
  const [error, setError] = useState<api.FetchError | null>(null);
  const needNewFetch = () => setFetchSeq((seq) => seq + 1);
  const snackbars = useSnackbars();

  const clickActivity = (activity: Activity) => {
    toggleShowMenu();
    setActivity(activity);
  };

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

  function fetchedToplevel(toplevel: api.ToplevelResponse | null) {
    if (toplevel !== null && toplevel.cameras.length > 0) {
      return (
        <>
          <Route
            path=""
            element={
              <ListActivity
                toplevel={toplevel}
                showSelectors={showListSelectors}
                timeZoneName={timeZoneName!}
              />
            }
          />
          <Route
            path="live"
            element={
              <LiveActivity
                cameras={toplevel.cameras}
                layoutIndex={multiviewLayoutIndex}
              />
            }
          />
        </>
      );
    }
  }

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
  let activityMenu = null;
  if (error === null && toplevel !== null && toplevel.cameras.length > 0) {
    switch (activity) {
      case "list":
        activityMenu = (
          <IconButton
            aria-label="selectors"
            onClick={toggleShowListSelectors}
            color="inherit"
            size="small"
          >
            <FilterList />
          </IconButton>
        );
        break;
      case "live":
        activityMenu = (
          <MultiviewChooser
            layoutIndex={multiviewLayoutIndex}
            onChoice={(value) => {
              setMultiviewLayoutIndex(value);
              setSearchParams({ layout: value.toString() });
            }}
          />
        );
        break;
    }
  }
  return (
    <>
      <AppBar position="static">
        <MoonfireMenu
          loginState={loginState}
          requestLogin={() => {
            setLoginState("user-requested-login");
          }}
          logout={logout}
          menuClick={toggleShowMenu}
          activityMenuPart={activityMenu}
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
            onClick={() => clickActivity("list")}
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
            onClick={() => clickActivity("live")}
            component={Link}
            to="/live"
          >
            <ListItemIcon>
              <Videocam />
            </ListItemIcon>
            <ListItemText primary="Live view (experimental)" />
          </ListItem>
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
      <Routes>{fetchedToplevel(toplevel)}</Routes>
    </>
  );
}

export default App;
