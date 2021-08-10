// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import Button from "@material-ui/core/Button";
import IconButton from "@material-ui/core/IconButton";
import Menu from "@material-ui/core/Menu";
import MenuItem from "@material-ui/core/MenuItem";
import { Theme } from "@material-ui/core/styles";
import { createStyles, makeStyles } from "@material-ui/styles";
import Toolbar from "@material-ui/core/Toolbar";
import Typography from "@material-ui/core/Typography";
import AccountCircle from "@material-ui/icons/AccountCircle";
import MenuIcon from "@material-ui/icons/Menu";
import React from "react";
import { LoginState } from "./App";
import { Session } from "./types";

const useStyles = makeStyles((theme: Theme) =>
  createStyles({
    title: {
      flexGrow: 1,
    },
    activity: {
      marginRight: theme.spacing(2),
    },
  })
);

interface Props {
  loginState: LoginState;
  setSession: (session: Session | null) => void;
  requestLogin: () => void;
  logout: () => void;
  menuClick?: () => void;
  activityMenuPart: JSX.Element | null;
}

// https://material-ui.com/components/app-bar/
function MoonfireMenu(props: Props) {
  const classes = useStyles();
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
        <Typography variant="h6" className={classes.title}>
          Moonfire NVR
        </Typography>
        {props.activityMenuPart !== null && (
          <div className={classes.activity}>{props.activityMenuPart}</div>
        )}
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
              <MenuItem onClick={handleLogout}>Logout</MenuItem>
            </Menu>
          </div>
        )}
      </Toolbar>
    </>
  );
}

export default MoonfireMenu;
