import projectLicenseText from "../../LICENSE?raw";
import featherLicenseText from "../assets/licenses/feather-LICENSE.txt?raw";
import interLicenseText from "../assets/fonts/inter/LICENSE.txt?raw";
import smileySansLicenseText from "../assets/fonts/smiley-sans/LICENSE.txt?raw";

export type LicenseNoticeId = "ssahdrify" | "inter" | "smiley-sans" | "feather";

export interface LicenseNotice {
  id: LicenseNoticeId;
  name: string;
  licenseId: string;
  source: string;
  text: string;
}

export const LICENSE_NOTICES: readonly LicenseNotice[] = [
  {
    id: "ssahdrify",
    name: "SSA HDRify",
    licenseId: "GPL-3.0-or-later",
    source: "https://github.com/koagaroon/ssaHdrify-tauri",
    text: projectLicenseText,
  },
  {
    id: "inter",
    name: "Inter",
    licenseId: "OFL-1.1",
    source: "https://github.com/rsms/inter",
    text: interLicenseText,
  },
  {
    id: "smiley-sans",
    name: "Smiley Sans (得意黑)",
    licenseId: "OFL-1.1",
    source: "https://github.com/atelier-anchor/smiley-sans",
    text: smileySansLicenseText,
  },
  {
    id: "feather",
    name: "Feather Icons",
    licenseId: "MIT",
    source: "https://github.com/feathericons/feather",
    text: featherLicenseText,
  },
] as const;
