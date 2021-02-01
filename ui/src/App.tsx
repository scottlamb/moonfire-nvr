// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import Container from "@material-ui/core/Container";
import React, { useEffect, useState } from "react";
import * as api from "./api";
import MoonfireMenu from "./AppMenu";
import Login from "./Login";
import { useSnackbars } from "./snackbars";
import { Session } from "./types";

type LoginState =
  | "logged-in"
  | "not-logged-in"
  | "server-requires-login"
  | "user-requested-login";

function App() {
  const [session, setSession] = useState<Session | null>(null);
  const [fetchSeq, setFetchSeq] = useState(0);
  const [loginState, setLoginState] = useState<LoginState>("not-logged-in");
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
        csrf: session!.csrf,
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
        setSession(null);
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
            resp.response.session === undefined ? "not-logged-in" : "logged-in"
          );
          setSession(resp.response.session || null);
      }
    };
    console.debug("Toplevel fetch num", fetchSeq);
    doFetch(abort.signal);
    return () => {
      console.log("Aborting toplevel fetch num", fetchSeq);
      abort.abort();
    };
  }, [fetchSeq]);

  return (
    <>
      <MoonfireMenu
        session={session}
        setSession={setSession}
        requestLogin={() => {
          setLoginState("user-requested-login");
        }}
        logout={logout}
      />
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
      {error != null && (
        <Container>
          <h2>Error querying server</h2>
          <pre>{error.message}</pre>
          <p>
            You may find more information in the Javascript console. Try
            reloading the page once you believe the problem is resolved.
          </p>
        </Container>
      )}
    </>
  );
}

export default App;
