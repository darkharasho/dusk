import plus from "@axiapps/axi-design/icons/plus.svg?raw";
import refresh from "@axiapps/axi-design/icons/refresh-cw.svg?raw";
import user from "@axiapps/axi-design/icons/user.svg?raw";
import clock from "@axiapps/axi-design/icons/clock.svg?raw";
import play from "@axiapps/axi-design/icons/play.svg?raw";
import upload from "@axiapps/axi-design/icons/upload.svg?raw";
import check from "@axiapps/axi-design/icons/circle-check.svg?raw";
import sliders from "@axiapps/axi-design/icons/settings-2.svg?raw";
import info from "@axiapps/axi-design/icons/info.svg?raw";

/**
 * The language's own icons, inlined so they take `currentColor`. Each is a
 * static file from the design package, never anything a user typed.
 */
const ICONS = { plus, refresh, user, clock, play, upload, check, sliders, info } as const;

export type IconName = keyof typeof ICONS;

export function Icon({ name }: { name: IconName }) {
  return (
    <span
      className="dusk-icon"
      aria-hidden="true"
      dangerouslySetInnerHTML={{ __html: ICONS[name].replace("<svg ", '<svg class="axi-icon" ') }}
    />
  );
}
