import "react";

declare module "react" {
  interface HTMLAttributes<T> {
    /** Native HTML inert support; missing from the React 18 type bundle in this project. */
    inert?: boolean;
  }
}
