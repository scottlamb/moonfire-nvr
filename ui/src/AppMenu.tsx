// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import Button from "@mui/material/Button";
import IconButton from "@mui/material/IconButton";
import Menu from "@mui/material/Menu";
import MenuItem from "@mui/material/MenuItem";
import { useTheme } from "@mui/material/styles";
import Toolbar from "@mui/material/Toolbar";
import Typography from "@mui/material/Typography";
import AccountCircle from "@mui/icons-material/AccountCircle";
import MenuIcon from "@mui/icons-material/Menu";
import React from "react";
import { LoginState } from "./App";
import Box from "@mui/material/Box";
import { CurrentMode, useThemeMode } from "./components/ThemeMode";
import Brightness2 from "@mui/icons-material/Brightness2";
import Brightness7 from "@mui/icons-material/Brightness7";
import BrightnessAuto from "@mui/icons-material/BrightnessAuto";
import Tooltip from "@mui/material/Tooltip";

interface Props {
  loginState: LoginState;
  requestLogin: () => void;
  logout: () => void;
  changePassword: () => void;
  menuClick?: () => void;
  activityMenuPart?: JSX.Element;
}

// https://material-ui.com/components/app-bar/
function MoonfireMenu(props: Props) {
  const { choosenTheme, changeTheme } = useThemeMode();
  const theme = useTheme();
  const [accountMenuAnchor, setAccountMenuAnchor] =
    React.useState<null | HTMLElement>(null);

  const handleMenu = (event: React.MouseEvent<HTMLElement>) => {
    setAccountMenuAnchor(event.currentTarget);
  };

  const handleClose = () => {
    setAccountMenuAnchor(null);
  };

  const handleLogout = () => {
    // Note this close should happen before `auth` toggles, or material-ui will
    // be unhappy about the anchor element not being part of the layout.
    handleClose();
    props.logout();
  };

  const handleChangePassword = () => {
    handleClose();
    props.changePassword();
  };

  return (
    <>
      <Toolbar variant="dense">
        <IconButton
          edge="start"
          color="inherit"
          aria-label="menu"
          onClick={props.menuClick}
        >
          <MenuIcon />
        </IconButton>
        <Typography variant="h6" sx={{ flexGrow: 1 }}>
          Moonfire NVR
        </Typography>
        {props.activityMenuPart !== null && (
          <Box sx={{ marginRight: theme.spacing(2) }}>
            {props.activityMenuPart}
          </Box>
        )}
        <Tooltip title="Toggle theme">
          <IconButton
            onClick={changeTheme}
            color="inherit"
            size="small"
          >
            {choosenTheme === CurrentMode.Light ? <Brightness7 /> : choosenTheme === CurrentMode.Dark ? <Brightness2 /> : <BrightnessAuto />}
          </IconButton>
        </Tooltip>
        {props.loginState !== "unknown" && props.loginState !== "logged-in" && (
          <Button color="inherit" onClick={props.requestLogin}>
            Log in
          </Button>
        )}
        {props.loginState === "logged-in" && (
          <div>
            <IconButton
              aria-label="account of current user"
              aria-controls="primary-search-account-menu"
              aria-haspopup="true"
              onClick={handleMenu}
              color="inherit"
              size="small"
            >
              <AccountCircle />
            </IconButton>
            <Menu
              anchorEl={accountMenuAnchor}
              keepMounted
              anchorOrigin={{
                vertical: "bottom",
                horizontal: "right",
              }}
              transformOrigin={{
                vertical: "top",
                horizontal: "right",
              }}
              open={Boolean(accountMenuAnchor)}
              onClose={handleClose}
            >
              <MenuItem onClick={handleChangePassword}>
                Change password
              </MenuItem>
              <MenuItem onClick={handleLogout}>Logout</MenuItem>
            </Menu>
          </div>
        )}
      </Toolbar>
    </>
  );
}

export default MoonfireMenu;
