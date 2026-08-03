export type AppStatus = "available" | "coming-soon";

export interface AppAssets {
  windows?: string;
  macos?: string;
  linux?: string;
}

export interface CatalogApp {
  id: string;
  name: string;
  tagline: string;
  description: string;
  features: string[];
  icon?: string;
  startedDate?: string;
  repo?: string;
  site?: string;
  changelog?: string;
  status: AppStatus;
  assets?: AppAssets;
}

export interface Catalog {
  schemaVersion: number;
  brand: string;
  apps: CatalogApp[];
}
