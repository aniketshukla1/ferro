// /next.html → / (the F1 flip), keeping deep-link query and hash. Loaded by next.html only.
location.replace(`./${location.search}${location.hash}`);
