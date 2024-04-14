// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import AppBar from "@mui/material/AppBar";
import Drawer from "@mui/material/Drawer";
import List from "@mui/material/List";
import ListItemButton from "@mui/material/ListItemButton";
import ListItemIcon from "@mui/material/ListItemIcon";
import ListItemText from "@mui/material/ListItemText";
import ListIcon from "@mui/icons-material/List";
import PeopleIcon from "@mui/icons-material/People";
import Videocam from "@mui/icons-material/Videocam";
import * as api from "../api";

import MoonfireMenu from "../AppMenu";
import { useReducer } from "react";
import { LoginState } from "../App";
import { Link } from "react-router-dom";

export default function Header({ loginState, logout, setChangePasswordOpen, activityMenuPart, setLoginState, toplevel }:
  {
    loginState: LoginState,
    logout: () => void,
    setChangePasswordOpen: React.Dispatch<React.SetStateAction<boolean>>,
    activityMenuPart?: JSX.Element,
    setLoginState: React.Dispatch<React.SetStateAction<LoginState>>,
    toplevel: api.ToplevelResponse | null
  }) {
  const [showMenu, toggleShowMenu] = useReducer((m: boolean) => !m, false);

  return (
    <>
      <AppBar position="sticky">
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
          <ListItemButton
            key="list"
            onClick={toggleShowMenu}
            component={Link}
            to="/"
          >
            <ListItemIcon>
              <ListIcon />
            </ListItemIcon>
            <ListItemText primary="List view" />
          </ListItemButton>
          <ListItemButton
            key="live"
            onClick={toggleShowMenu}
            component={Link}
            to="/live"
          >
            <ListItemIcon>
              <Videocam />
            </ListItemIcon>
            <ListItemText primary="Live view (experimental)" />
          </ListItemButton>
          {toplevel?.permissions.adminUsers && (
            <ListItemButton
              key="users"
              onClick={toggleShowMenu}
              component={Link}
              to="/users"
            >
              <ListItemIcon>
                <PeopleIcon />
              </ListItemIcon>
              <ListItemText primary="Users" />
            </ListItemButton>
          )}
        </List>
      </Drawer>
    </>
  )
}