CREATE TABLE widgets (
    id text PRIMARY KEY,
    owner_id text,
    name text NOT NULL
);

CREATE INDEX widgets_owner_id ON widgets(owner_id);
