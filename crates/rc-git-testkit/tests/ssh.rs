//! The ssh server proves itself against git and OpenSSH: clone from it, push to it.

use rc_git_testkit::{Fixture, SshGitServer};

#[test]
fn a_served_repository_clones_and_accepts_a_push_over_openssh() {
    let origin = Fixture::standard();
    let (bare, bare_path) = origin.bare_copy();
    let server = SshGitServer::serve(&bare_path);

    let ssh_dir = Fixture::empty_dir();
    server.write_client_key(ssh_dir.path());
    server.write_known_hosts(ssh_dir.path());
    let dir = ssh_dir.path().display();
    // `Fixture` clears the environment, so the command reaches git only through
    // `git_with_env`. `-F /dev/null` and the agent opt-out keep the user's ssh setup out.
    let ssh_command = format!(
        "ssh -F /dev/null -i {dir}/id_ed25519 -o UserKnownHostsFile={dir}/known_hosts \
         -o StrictHostKeyChecking=yes -o IdentitiesOnly=yes -o IdentityAgent=none \
         -o BatchMode=yes"
    );
    let env = [("GIT_SSH_COMMAND", ssh_command.as_str())];

    let clone = Fixture::empty_dir();
    clone.git_with_env(&["clone", "--quiet", &server.url(), "."], &env);
    assert_eq!(clone.rev_parse("HEAD"), origin.rev_parse("HEAD"));

    clone.write("pushed.txt", b"pushed\n");
    clone.commit("Pushed over ssh", "2024-02-01T00:00:00+0000");
    clone.git_with_env(&["push", "--quiet"], &env);

    let served_tip = bare.git_output(&["--git-dir", "remote.git", "rev-parse", "main"]);
    assert_eq!(clone.rev_parse("HEAD"), served_tip);
}
