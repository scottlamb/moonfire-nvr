// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import { useColorScheme } from "@mui/material/styles";
import React, { createContext } from "react";

export enum CurrentMode {
  Auto = 0,
  Light = 1,
  Dark = 2,
}

interface ThemeProps {
  changeTheme: () => void;
  currentTheme: "dark" | "light";
  choosenTheme: CurrentMode;
}

export const ThemeContext = createContext<ThemeProps>({
  currentTheme: window.matchMedia("(prefers-color-scheme: dark)").matches
    ? "dark"
    : "light",
  changeTheme: () => {},
  choosenTheme: CurrentMode.Auto,
});

const ThemeMode = ({ children }: { children: JSX.Element }): JSX.Element => {
  const { mode, setMode } = useColorScheme();

  const useMediaQuery = (query: string) => {
    const [matches, setMatches] = React.useState(
      () => window.matchMedia(query).matches,
    );
    React.useEffect(() => {
      const m = window.matchMedia(query);
      const l = () => setMatches(m.matches);
      m.addEventListener("change", l);
      return () => m.removeEventListener("change", l);
    }, [query]);
    return matches;
  };

  const detectedSystemColorScheme = useMediaQuery(
    "(prefers-color-scheme: dark)",
  )
    ? "dark"
    : "light";

  const changeTheme = React.useCallback(() => {
    setMode(mode === "dark" ? "light" : mode === "light" ? "system" : "dark");
  }, [mode, setMode]);

  const currentTheme =
    mode === "system"
      ? detectedSystemColorScheme
      : (mode ?? detectedSystemColorScheme);
  const choosenTheme =
    mode === "dark"
      ? CurrentMode.Dark
      : mode === "light"
        ? CurrentMode.Light
        : CurrentMode.Auto;

  return (
    <ThemeContext.Provider value={{ changeTheme, currentTheme, choosenTheme }}>
      {children}
    </ThemeContext.Provider>
  );
};

export default ThemeMode;

export const useThemeMode = () => React.useContext(ThemeContext);
