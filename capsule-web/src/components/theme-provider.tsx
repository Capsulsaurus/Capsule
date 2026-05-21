import { createContext, useContext, useEffect, useState } from 'react';

type Theme = 'dark' | 'light' | 'system';

type ThemeProviderProps = {
    children: React.ReactNode;
    defaultTheme?: Theme;
    storageKey?: string;
};

type ThemeProviderState = {
    theme: Theme;
    setTheme: (theme: Theme) => void;
};

const initialState: ThemeProviderState = {
    theme: 'system',
    setTheme: () => null,
};

// TODO: Currently doesn't work with Tailwind v4. Need to fix.

const ThemeProviderContext = createContext<ThemeProviderState>(initialState);

export function ThemeProvider({
    children,
    defaultTheme = 'system',
    storageKey = 'capsule-ui-theme',
    ...props
}: ThemeProviderProps) {
    const [theme, setTheme] = useState<Theme>(
        () => (localStorage.getItem(storageKey) as Theme) || defaultTheme,
    );

    useEffect(() => {
        // This is the key change for Tailwind v4 compatibility
        if (theme === 'dark') {
            document.documentElement.classList.add('dark');
            localStorage.theme = 'dark';
        } else if (theme === 'light') {
            document.documentElement.classList.remove('dark');
            localStorage.theme = 'light';
        } else {
            // "system" - follow OS preference
            const systemIsDark = window.matchMedia(
                '(prefers-color-scheme: dark)',
            ).matches;
            document.documentElement.classList.toggle('dark', systemIsDark);
            localStorage.removeItem('theme'); // Remove theme from localStorage
        }
    }, [theme]);

    // Also add a listener for OS theme changes when in "system" mode
    useEffect(() => {
        if (theme !== 'system') return;

        const mediaQuery = window.matchMedia('(prefers-color-scheme: dark)');

        const handleChange = () => {
            document.documentElement.classList.toggle(
                'dark',
                mediaQuery.matches,
            );
        };

        mediaQuery.addEventListener('change', handleChange);
        return () => mediaQuery.removeEventListener('change', handleChange);
    }, [theme]);

    const value = {
        theme,
        setTheme: (newTheme: Theme) => {
            if (newTheme === 'system') {
                localStorage.removeItem(storageKey);
            } else {
                localStorage.setItem(storageKey, newTheme);
            }
            setTheme(newTheme);
        },
    };

    return (
        <ThemeProviderContext.Provider {...props} value={value}>
            {children}
        </ThemeProviderContext.Provider>
    );
}

// export function ThemeProvider({
//     children,
//     defaultTheme = "system",
//     storageKey = "theme",
//     ...props
// }: ThemeProviderProps) {
//     const [theme, setTheme] = useState<Theme>(
//         () => (localStorage.getItem(storageKey) as Theme) || defaultTheme
//     )

//     useEffect(() => {
//         const root = window.document.documentElement

//         root.classList.remove("light", "dark")

//         if (theme === "system") {
//             const systemTheme = window.matchMedia("(prefers-color-scheme: dark)")
//                 .matches
//                 ? "dark"
//                 : "light"

//             root.classList.add(systemTheme)
//             return
//         }

//         root.classList.add(theme)
//     }, [theme])

//     const value = {
//         theme,
//         setTheme: (theme: Theme) => {
//             localStorage.setItem(storageKey, theme)
//             setTheme(theme)
//         },
//     }

//     return (
//         <ThemeProviderContext.Provider {...props} value={value}>
//             {children}
//         </ThemeProviderContext.Provider>
//     )
// }

export const useTheme = () => {
    const context = useContext(ThemeProviderContext);

    if (context === undefined)
        throw new Error('useTheme must be used within a ThemeProvider');

    return context;
};
