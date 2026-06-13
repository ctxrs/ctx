import { getFileIconSrc } from "../utils/fileIcons";

interface FileIconProps {
  path: string;
  size?: number;
  className?: string;
}

export function FileIcon({ path, size = 16, className = "" }: FileIconProps) {
  const iconSrc = getFileIconSrc(path);

  return (
    <img
      src={iconSrc}
      alt=""
      width={size}
      height={size}
      className={className}
      style={{
        display: "block",
        flexShrink: 0,
        filter: "brightness(0) saturate(100%) invert(59%) sepia(11%) saturate(1387%) hue-rotate(162deg) brightness(91%) contrast(87%)",
      }}
    />
  );
}
