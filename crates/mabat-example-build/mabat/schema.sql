CREATE TABLE project (
    id INTEGER PRIMARY KEY,
    name VARCHAR(100) NOT NULL,
    description TEXT
);
CREATE TABLE task (
    id BIGINT PRIMARY KEY,
    project_id BIGINT NOT NULL REFERENCES project (id),
    title VARCHAR(200) NOT NULL,
    done BOOLEAN NOT NULL
);
