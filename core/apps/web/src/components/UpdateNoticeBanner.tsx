import { UpdateNoticeBannerView } from "./updateNotice/UpdateNoticeBannerView";
import {
  useUpdateNoticeBanner,
  type UpdateNoticeBannerProps,
} from "./updateNotice/useUpdateNoticeBanner";

export default function UpdateNoticeBanner(props: UpdateNoticeBannerProps) {
  const model = useUpdateNoticeBanner(props);
  return <UpdateNoticeBannerView {...model} />;
}
