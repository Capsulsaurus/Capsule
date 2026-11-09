import { createLazyFileRoute, Link } from '@tanstack/react-router';
import { useMemo } from 'react';
import { FormattedMessage, useIntl } from 'react-intl';
import {
    Card,
    CardContent,
    CardDescription,
    CardHeader,
} from '@/components/ui/card';
import { Label } from '@/components/ui/label';
import { languageOptions } from '@/i18n/locale';
import { useLocaleSettings } from '@/i18n/locale-settings';
import { regionOptions } from '@/i18n/region';

export const Route = createLazyFileRoute('/settings/language')({
    component: LanguageSettings,
});

/** The value a picker uses for "follow the OS"; never a real tag or region code. */
const SYSTEM = '';

const SELECT_CLASS =
    'h-9 w-full rounded-md border border-input bg-transparent px-3 py-1 text-sm shadow-xs focus-visible:outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50';

function LanguageSettings() {
    const intl = useIntl();
    const settings = useLocaleSettings();

    const languages = useMemo(() => languageOptions(), []);
    // Region names are shown in the UI language, not the region's own.
    const regions = useMemo(
        () => regionOptions(settings.language),
        [settings.language],
    );

    const systemLanguageName =
        languages.find((option) => option.code === settings.systemLanguage)
            ?.name ?? settings.systemLanguage;
    const systemRegionName = settings.systemRegion
        ? (regions.find((option) => option.code === settings.systemRegion)
              ?.name ?? settings.systemRegion)
        : undefined;

    return (
        <div className="max-w-2xl mx-auto p-6 space-y-8">
            <div className="flex items-center justify-between">
                <h1 className="text-2xl font-bold">
                    <FormattedMessage id="settings.language.title" />
                </h1>
                <Link
                    to="/settings"
                    className="text-sm underline text-muted-foreground"
                >
                    <FormattedMessage id="settings.language.profile_link" />
                </Link>
            </div>

            <Card>
                <CardHeader>
                    <CardDescription>
                        <FormattedMessage id="settings.language.description" />
                    </CardDescription>
                </CardHeader>
                <CardContent className="space-y-4">
                    <div className="grid gap-2">
                        <Label htmlFor="language">
                            <FormattedMessage id="settings.language.language_label" />
                        </Label>
                        <select
                            id="language"
                            className={SELECT_CLASS}
                            value={settings.languagePreference ?? SYSTEM}
                            onChange={(e) =>
                                settings.setLanguagePreference(
                                    e.target.value === SYSTEM
                                        ? undefined
                                        : e.target.value,
                                )
                            }
                        >
                            <option value={SYSTEM}>
                                {intl.formatMessage(
                                    { id: 'settings.language.system_default' },
                                    { name: systemLanguageName },
                                )}
                            </option>
                            {languages.map((option) => (
                                <option
                                    key={option.code}
                                    value={option.code}
                                    lang={option.code}
                                >
                                    {option.name}
                                </option>
                            ))}
                        </select>
                    </div>
                    <div className="grid gap-2">
                        <Label htmlFor="region">
                            <FormattedMessage id="settings.language.region_label" />
                        </Label>
                        <select
                            id="region"
                            className={SELECT_CLASS}
                            value={settings.regionPreference ?? SYSTEM}
                            onChange={(e) =>
                                settings.setRegionPreference(
                                    e.target.value === SYSTEM
                                        ? undefined
                                        : e.target.value,
                                )
                            }
                        >
                            <option value={SYSTEM}>
                                {systemRegionName
                                    ? intl.formatMessage(
                                          {
                                              id: 'settings.language.system_default',
                                          },
                                          { name: systemRegionName },
                                      )
                                    : intl.formatMessage({
                                          id: 'settings.language.system_unknown',
                                      })}
                            </option>
                            {regions.map((option) => (
                                <option key={option.code} value={option.code}>
                                    {option.name}
                                </option>
                            ))}
                        </select>
                    </div>
                </CardContent>
            </Card>
        </div>
    );
}
