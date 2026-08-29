# Project Guidelines

## Git commit guidelines

### Commit types

The subject line must begin with one of these types, followed by `: `:

- `feat`: A new feature
- `fix`: A bug fix
- `docs`: Documentation-only changes
- `style`: Changes that do not affect the meaning of the code
- `refactor`: A change that neither fixes a bug nor adds a feature
- `perf`: A change that improves performance
- `test`: Adding or correcting tests
- `chore`: Build process or auxiliary tool changes
- `ci`: CI configuration files and scripts

### Best practices

- Commit early and often, but never push automatically.
- Run `./scripts/check.sh` and require it to pass before committing.
- Use the imperative mood, such as `add` rather than `added` or `adds`.
- Capitalize the subject line.
- Do not end the subject line with a period.
- Limit the subject line to 50 characters.
- Separate the subject from the body with a blank line.
- Use the body to explain what and why, not how.
- Wrap the body at 72 characters.
- Do not add agent or platform footer text manually.

### Commit format

```text
<type>[scope]: <description>

[body]
- Bullet point for change 1
- Bullet point for change 2
```