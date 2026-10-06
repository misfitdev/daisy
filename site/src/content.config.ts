import { defineCollection } from "astro:content";
import { glob } from "astro/loaders";

const docs = defineCollection({
  loader: glob({ pattern: "*.md", base: "../docs" }),
});

const contributors = defineCollection({
  loader: glob({ pattern: "CONTRIBUTING.md", base: ".." }),
});
export const collections = { docs, contributors };
