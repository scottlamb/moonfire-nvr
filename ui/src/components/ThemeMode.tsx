import { useColorScheme } from "@mui/material";
import React, { createContext } from "react";

interface ThemeProps {
  changeTheme: () => void,
  currentTheme?: 'dark' | 'light',
  getTheme: 0 | 1 | 2,
  systemColor: 'dark' | 'light'
}

export const ThemeContext = createContext<ThemeProps>({
  currentTheme: window.matchMedia("(prefers-color-scheme: dark)").matches ? 'dark' : 'light',
  changeTheme: () => { },
  getTheme: 0,
  systemColor: window.matchMedia("(prefers-color-scheme: dark)").matches ? 'dark' : 'light'
});

const ThemeMode = ({ children }: { children: JSX.Element }): JSX.Element => {
  const { mode, setMode } = useColorScheme();
  const [detectedSystemColorScheme, setDetectedSystemColorScheme] = React.useState<'dark' | 'light'>(
    window.matchMedia("(prefers-color-scheme: dark)").matches ? 'dark' : 'light'
  );

  React.useEffect(() => {
    window.matchMedia("(prefers-color-scheme: dark)")
      .addEventListener("change", (e) => {
        setDetectedSystemColorScheme(e.matches ? 'dark' : 'light');
      });
  }, []);

  const changeTheme = React.useCallback(() => {
    setMode(mode === 'dark' ? 'light' : mode === 'light' ? 'system' : 'dark')
  }, [mode]);

  const currentTheme = mode === 'system' ? detectedSystemColorScheme : mode;
  const getTheme = mode === 'dark' ? 2 : mode === 'light' ? 1 : 0;

  return (
    <ThemeContext.Provider value={{ changeTheme, currentTheme, getTheme, systemColor: detectedSystemColorScheme }}>
      {children}
    </ThemeContext.Provider>
  )
}

export default ThemeMode;

export const useThemeMode = () => React.useContext(ThemeContext);