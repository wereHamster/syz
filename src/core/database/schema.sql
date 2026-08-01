-- A project corresponds roughly to a git repository. We track on which platform
-- the repository is hosted, though currently we only support GitHub.
CREATE TABLE project (
    id text PRIMARY KEY,

    platform text NOT NULL,
    repository text NOT NULL
);

-- A scan is an atomic snapshot of the project dependencies at a particular time.
CREATE TABLE scan (
    id text PRIMARY KEY,
    project_id text REFERENCES project(id),

    create_time timestamp NOT NULL
);

-- A dependency of a project, at the time of the associated scan. The specifier
-- is from the package.json file (which may define a precise version or a range).
-- The referenced package is the actual version that's specified by the lock file.
CREATE TABLE dependency (
    id text PRIMARY KEY,
    scan_id text REFERENCES scan(id),

    specifier text,
    package_id text REFERENCES package(id)
);

-- A package, always referencing a specific version.
CREATE TABLE package (
    id text PRIMARY KEY,

    -- Components of Package-URL (PURL).
    type text NOT NULL,
    namespace text,
    name text NOT NULL,
    version text NOT NULL,
    subpath text
);

-- A package or group of packages that are out of date and can be bumped.
-- All bumps are relevant to a specific project, based on the most recent
-- scan.
CREATE TABLE bump (
    id text PRIMARY KEY,
    project_id text REFERENCES project(id),

    -- The name of the dependency (single-package bump) or the name of the
    -- group of this bump contains multiple packages.
    name text NOT NULL,

    -- Whether this bump is a major version bump.
    major boolean NOT NULL,

    -- If approved, the code change is scheduled and performed in the
    -- background.
    approved boolean NOT NULL,

    -- URL to the GitHub Pull Request.
    url text
);

-- The join table to connect a bump with the dependencies. For single-package
-- bumps there will only be one bumpdep per bump. For multi-package bumps
-- there will be many.
CREATE TABLE bumpdep (
    bump_id text REFERENCES bump(id),
    dependency_id text REFERENCES dependency(id),

    -- The version we want to update to.
    target_version text NOT NULL,

    -- The absolute latest available version.
    head_version text NOT NULL,

    -- Minimum release age in seconds
    minimum_release_age integer
);
