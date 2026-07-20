import { useLocale } from "./i18n";
import { Icon } from "./ui/icons";
import { IconButton, Tooltip } from "./ui/primitives";

export function LocaleToggle() {
  const { t, toggleLocale } = useLocale();
  return (
    <Tooltip label={t("language")}>
      <IconButton
        aria-label={t("language")}
        icon={<Icon name="language" />}
        type="button"
        onClick={toggleLocale}
      />
    </Tooltip>
  );
}
