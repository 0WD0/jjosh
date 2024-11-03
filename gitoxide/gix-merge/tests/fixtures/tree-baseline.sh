#!/usr/bin/env bash
set -eu -o pipefail

function tick () {
  if test -z "${tick+set}"
  then
    tick=1112911993
  else
    tick=$(($tick + 60))
  fi
  GIT_COMMITTER_DATE="$tick -0700"
  GIT_AUTHOR_DATE="$tick -0700"
  export GIT_COMMITTER_DATE GIT_AUTHOR_DATE
}

function write_lines () {
	printf "%s\n" "$@"
}

function seq () {
	case $# in
	1)	set 1 "$@" ;;
	2)	;;
	*)	{ echo "need 1 or 2 parameters: <end> or <start> <end>" 1>&2 && exit 2; } ;;
	esac
	local seq_counter=$1
	while test "$seq_counter" -le "$2"
	do
		echo "$seq_counter"
		seq_counter=$(( seq_counter + 1 ))
	done
}

function baseline () (
  local dir=${1:?the directory to enter}
  local output_name=${2:?the basename of the output of the merge}
  local our_committish=${3:?our side from which a commit can be derived}
  local their_committish=${4:?Their side from which a commit can be derived}
  local opt_deviation_message=${5:-}
  local one_side=${6:-}

  cd "$dir"
  local our_commit_id
  local their_commit_id

  local conflict_style="merge"
   if [[ "$output_name" == *-merge ]]; then
       conflict_style="merge"
   elif [[ "$output_name" == *-diff3 ]]; then
       conflict_style="diff3"
   fi

  our_commit_id="$(git rev-parse "$our_committish")"
  their_commit_id="$(git rev-parse "$their_committish")"
  local maybe_expected_tree="$(git rev-parse expected^{tree})"
  if [ -z "$opt_deviation_message" ]; then
    maybe_expected_tree="expected^{tree}"
  fi

  local merge_info="${output_name}.merge-info"
  git -c merge.conflictStyle=$conflict_style merge-tree -z --write-tree --allow-unrelated-histories "$our_committish" "$their_committish" > "$merge_info" || :
  echo "$dir" "$conflict_style" "$our_commit_id" "$our_committish" "$their_commit_id" "$their_committish" "$merge_info" "$maybe_expected_tree" "$opt_deviation_message" >> ../baseline.cases

  if [[ "$one_side" != "no-reverse" ]]; then
    local merge_info="${output_name}-reversed.merge-info"
    git -c merge.conflictStyle=$conflict_style merge-tree -z --write-tree --allow-unrelated-histories "$their_committish" "$our_committish" > "$merge_info" || :
    echo "$dir" "$conflict_style" "$their_commit_id" "$their_committish" "$our_commit_id" "$our_committish" "$merge_info" "$maybe_expected_tree" "$opt_deviation_message" >> ../baseline.cases
  fi
)

git init simple
(cd simple
  rm -Rf .git/hooks
  write_lines 1 2 3 4 5 >numbers
  echo hello >greeting
  echo foo >whatever
  git add numbers greeting whatever
  tick
  git commit -m initial

  git branch side1
  git branch side2
  git branch side3
  git branch side4

  git checkout side1
  write_lines 1 2 3 4 5 6 >numbers
  echo hi >greeting
  echo bar >whatever
  git add numbers greeting whatever
  tick
  git commit -m modify-stuff

  git checkout side2
  write_lines 0 1 2 3 4 5 >numbers
  echo yo >greeting
  git rm whatever
  mkdir whatever
  >whatever/empty
  git add numbers greeting whatever/empty
  tick
  git commit -m other-modifications

  git checkout side3
  git mv numbers sequence
  tick
  git commit -m rename-numbers

  git checkout side4
  write_lines 0 1 2 3 4 5 >numbers
  echo yo >greeting
  git add numbers greeting
  tick
  git commit -m other-content-modifications

  git switch --orphan unrelated
  >something-else
  git add something-else
  tick
  git commit -m first-commit

  git checkout -b tweak1 side1
  write_lines zero 1 2 3 4 5 6 >numbers
  git add numbers
  git mv numbers "Αυτά μου φαίνονται κινέζικα"
  git commit -m "Renamed numbers"
)

git init rename-delete
(cd rename-delete
  write_lines 1 2 3 4 5 >foo
  mkdir olddir
  for i in a b c; do echo $i >olddir/$i; done
  git add foo olddir
  git commit -m "original"

  git branch A
  git branch B

  git checkout A
  write_lines 1 2 3 4 5 6 >foo
  git add foo
  git mv olddir newdir
  git commit -m "Modify foo, rename olddir to newdir"

  git checkout B
  write_lines 1 2 3 4 5 six >foo
  git add foo
  git mv foo olddir/bar
  git commit -m "Modify foo & rename foo -> olddir/bar"
)

git init rename-add
(cd rename-add
		write_lines original 1 2 3 4 5 >foo
		git add foo
		git commit -m "original"

		git branch A
		git branch B

		git checkout A
		write_lines 1 2 3 4 5 >foo
		echo "different file" >bar
		git add foo bar
		git commit -m "Modify foo, add bar"

		git checkout B
		write_lines original 1 2 3 4 5 6 >foo
		git add foo
		git mv foo bar
		git commit -m "rename foo to bar"
)

git init rename-add-symlink
(cd rename-add-symlink
  write_lines original 1 2 3 4 5 >foo
  git add foo
  git commit -m "original"

  git branch A
  git branch B

  git checkout A
  write_lines 1 2 3 4 5 >foo
  ln -s foo bar
  git add foo bar
  git commit -m "Modify foo, add symlink bar"

  git checkout B
  write_lines original 1 2 3 4 5 6 >foo
  git add foo
  git mv foo bar
  git commit -m "rename foo to bar"
)

git init rename-rename-plus-content
(cd rename-rename-plus-content
  write_lines 1 2 3 4 5 >foo
  git add foo
  git commit -m "original"

  git branch A
  git branch B

  git checkout A
  write_lines 1 2 3 4 5 six >foo
  git add foo
  git mv foo bar
  git commit -m "Modify foo + rename to bar"

  git checkout B
  write_lines 1 2 3 4 5 6 >foo
  git add foo
  git mv foo baz
  git commit -m "Modify foo + rename to baz"
)

git init rename-add-delete
(
  cd rename-add-delete
  echo "original file" >foo
  git add foo
  git commit -m "original"

  git branch A
  git branch B

  git checkout A
  git rm foo
  echo "different file" >bar
  git add bar
  git commit -m "Remove foo, add bar"

  git checkout B
  git mv foo bar
  git commit -m "rename foo to bar"
)

git init rename-rename-delete-delete
(
  cd rename-rename-delete-delete
  echo foo >foo
  echo bar >bar
  git add foo bar
  git commit -m O

  git branch A
  git branch B

  git checkout A
  git mv foo baz
  git rm bar
  git commit -m "Rename foo, remove bar"

  git checkout B
  git mv bar baz
  git rm foo
  git commit -m "Rename bar, remove foo"
)

git init super-1
(cd super-1
  seq 11 19 >one
  seq 31 39 >three
  seq 51 59 >five
  git add .
  tick
  git commit -m "O"

  git branch A
  git branch B

  git checkout A
  seq 10 19 >one
  echo 40        >>three
  git add one three
  git mv  one   two
  git mv  three four
  git mv  five  six
  tick
  git commit -m "A"

  git checkout B
  echo 20    >>one
  echo forty >>three
  echo 60    >>five
  git add one three five
  git mv  one   six
  git mv  three two
  git mv  five  four
  tick
  git commit -m "B"
)

git init super-2
(cd super-2
  write_lines 1 2 3 4 5 >foo
  mkdir olddir
  for i in a b c; do echo $i >olddir/$i || exit 1; done
  git add foo olddir
  git commit -m "original"

  git branch A
  git branch B

  git checkout A
  git rm foo
  git mv olddir newdir
  mkdir newdir/bar
  >newdir/bar/file
  git add newdir/bar/file
  git commit -m "rm foo, olddir/ -> newdir/, + newdir/bar/file"

  git checkout B
  write_lines 1 2 3 4 5 6 >foo
  git add foo
  git mv foo olddir/bar
  git commit -m "Modify foo & rename foo -> olddir/bar"
)

git init rename-within-rename
(cd rename-within-rename
  mkdir a && write_lines original 1 2 3 4 5 >a/x.f
  mkdir a/sub && write_lines original 1 2 3 4 5 >a/sub/y.f
  touch a/w a/sub/z
  git add . && git commit -m "original"

  git branch A
  git branch B
  git branch expected

  git checkout A
  write_lines 1 2 3 4 5 >a/x.f
  write_lines 1 2 3 4 5 >a/sub/y.f
  git mv a a-renamed
  git commit -am "changed all content, renamed a -> a-renamed"

  git checkout B
  write_lines original 1 2 3 4 5 6 >a/x.f
  write_lines original 1 2 3 4 5 6 >a/sub/y.f
  git mv a/sub a/sub-renamed
  git commit -am "changed all content, renamed a/sub -> a/sub-renamed"

  git checkout expected
  write_lines 1 2 3 4 5 6 >a/x.f
  write_lines 1 2 3 4 5 6 >a/sub/y.f
  git mv a/sub a/sub-renamed
  git mv a a-renamed
  git commit -am "tracked both renames, applied all modifications by merge"
)

git init rename-within-rename-2
(cd rename-within-rename-2
  mkdir a && write_lines original 1 2 3 4 5 >a/x.f
  mkdir a/sub && write_lines original 1 2 3 4 5 >a/sub/y.f
  touch a/w a/sub/z
  git add . && git commit -m "original"

  git branch A
  git branch B
  git branch expected

  git checkout A
  write_lines 1 2 3 4 5 >a/x.f
  write_lines 1 2 3 4 5 >a/sub/y.f
  git mv a/sub a/sub-renamed
  git mv a a-renamed
  git commit -am "changed all content, renamed a -> a-renamed, a/sub -> a/sub-renamed"

  git checkout B
  write_lines original 1 2 3 4 5 6 >a/x.f
  write_lines original 1 2 3 4 5 6 >a/sub/y.f
  git mv a/sub a/sub-renamed
  git commit -am "changed all content, renamed a/sub -> a/sub-renamed"

  git checkout expected
  write_lines 1 2 3 4 5 6 >a/x.f
  write_lines 1 2 3 4 5 6 >a/sub/y.f
  git mv a/sub a/sub-renamed
  git mv a a-renamed
  git commit -am "tracked both renames, applied all modifications by merge"
)

git init conflicting-rename
(cd conflicting-rename
  mkdir a && write_lines original 1 2 3 4 5 >a/x.f
  mkdir a/sub && write_lines original 1 2 3 4 5 >a/sub/y.f
  touch a/w a/sub/z
  git add . && git commit -m "original"

  git branch A
  git branch B

  git checkout A
  write_lines 1 2 3 4 5 >a/x.f
  write_lines 1 2 3 4 5 >a/sub/y.f
  git mv a a-renamed
  git commit -am "changed all content, renamed a -> a-renamed"

  git checkout B
  write_lines original 1 2 3 4 5 6 >a/x.f
  write_lines original 1 2 3 4 5 6 >a/sub/y.f
  git mv a a-different
  git commit -am "changed all content, renamed a -> a-different"
)

git init conflicting-rename-2
(cd conflicting-rename-2
  mkdir a && write_lines original 1 2 3 4 5 >a/x.f
  mkdir a/sub && write_lines original 1 2 3 4 5 >a/sub/y.f
  touch a/w a/sub/z
  git add . && git commit -m "original"

  git branch A
  git branch B

  git checkout A
  write_lines 1 2 3 4 5 >a/x.f
  write_lines 1 2 3 4 5 >a/sub/y.f
  git mv a/sub a/sub-renamed
  git commit -am "changed all content, renamed a/sub -> a/sub-renamed"

  git checkout B
  write_lines original 1 2 3 4 5 6 >a/x.f
  write_lines original 1 2 3 4 5 6 >a/sub/y.f
  git mv a/sub a/sub-different
  git commit -am "changed all content, renamed a/sub -> a/sub-different"
)

git init conflicting-rename-complex
(cd conflicting-rename-complex
  mkdir a && write_lines original 1 2 3 4 5 >a/x.f
  mkdir a/sub && write_lines original 1 2 3 4 5 >a/sub/y.f
  touch a/w a/sub/z
  git add . && git commit -m "original"

  git branch A
  git branch B
  git branch expected

  git checkout A
  write_lines 1 2 3 4 5 >a/x.f
  write_lines 1 2 3 4 5 >a/sub/y.f
  git mv a a-renamed
  git commit -am "changed all content, renamed a -> a-renamed"

  git checkout B
  write_lines original 1 2 3 4 5 6 >a/sub/y.f
  git mv a/sub tmp
  git rm -r a
  git mv tmp a
  git commit -am "change something in subdirectory, then overwrite directory with subdirectory"

  git checkout expected
  rm .git/index
  rm -Rf ./a
  mkdir -p a-renamed/sub
  write_lines 1 2 3 4 5 >a-renamed/sub/y.f
  write_lines 1 2 3 4 5 6 >a-renamed/y.f
  touch a-renamed/z a-renamed/sub/z
  git add .
  git commit -m "Close to what Git has, but different due to rename tracking (which looses 'a/w', and 'x.f' becomes y.f). But the merge is so 'erroneous' that it's beyond rescue"
)

git init same-rename-different-mode
(cd same-rename-different-mode
  mkdir a && write_lines original 1 2 3 4 5 >a/x.f
  touch a/w
  git add . && git commit -m "original"

  git branch A
  git branch B
  git branch expected

  git checkout A
  write_lines 1 2 3 4 5 >a/x.f
  chmod +x a/x.f a/w
  git update-index --chmod=+x a/x.f a/w
  git mv a a-renamed
  git commit -am "changed all content, add +x, renamed a -> a-renamed"

  git checkout B
  write_lines original 1 2 3 4 5 6 >a/x.f
  git mv a a-renamed
  git commit -am "changed all content, renamed a -> a-renamed"

  git checkout expected
  chmod +x a/x.f a/w
  git update-index --chmod=+x a/x.f a/w
  write_lines 1 2 3 4 5 6 >a/x.f
  git mv a a-renamed
  git commit -am "Git, when branches are reversed, doesn't keep the +x flag on a/w so we specify our own expectation"
)

git init renamed-symlink-with-conflict
(cd renamed-symlink-with-conflict
  mkdir a && write_lines original 1 2 3 4 5 >a/x.f
  ln -s a/x.f link
  git add . && git commit -m "original"

  git branch A
  git branch B

  git checkout A
  write_lines 1 2 3 4 5 >a/x.f
  git mv link link-renamed
  git commit -am "changed a/x.f, renamed link -> link-renamed"

  git checkout B
  write_lines original 1 2 3 4 5 6 >a/x.f
  git mv link link-different
  git commit -am "change content, renamed link -> link-different"
)

git init added-file-changed-content-and-mode
(cd added-file-changed-content-and-mode
  mkdir a && write_lines original 1 2 3 4 5 >a/x.f
  git add . && git commit -m "original"

  git branch A
  git branch B
  git branch expected

  git checkout A
  write_lines 1 2 3 4 5 >new
  git add .
  git commit -m "add 'new' with content A"

  git checkout B
  write_lines original 1 2 3 4 5 6 >new
  chmod +x new
  git add .
  git commit -m "add new with content B and +x"

  git checkout expected
  echo -n $'<<<<<<< A\n1\n2\n3\n4\n5\n=======\noriginal\n1\n2\n3\n4\n5\n6\n>>>>>>> B\n' >new
  chmod +x new
  git add new
  git commit -m "Git has a better merge here, but that's due to better hunk handling/hunk splitting. We, however, consistently use +x"
)

git init type-change-and-renamed
(cd type-change-and-renamed
  mkdir a && >a/x.f
  ln -s a/x.f link
  git add . && git commit -m "original"

  git branch A
  git branch B

  git checkout A
  rm link && echo not-link > link
  git commit -am "link type-changed, file changed"

  git checkout B
  git mv link link-renamed
  git commit -am "just renamed the link"
)

git init change-and-delete
(cd change-and-delete
  mkdir a && write_lines original 1 2 3 4 5 >a/x.f
  ln -s a/x.f link
  git add . && git commit -m "original"

  git branch A
  git branch B

  git checkout A
  write_lines 1 2 3 4 5 6 >a/x.f
  rm link && echo not-link > link
  git commit -am "link type-changed, file changed"

  git checkout B
  git rm link a/x.f
  git commit -am "delete everything"
)

git init submodule-both-modify
(cd submodule-both-modify
	mkdir sub
	(cd sub
	 git init
	 echo original > file
	 git add file
	 tick
	 git commit -m sub-root
	)
	git add sub
	tick
	git commit -m root

	git branch expected

	git checkout -b A main
	(cd sub
	 echo A > file
	 git add file
	 tick
	 git commit -m sub-a
	)
	git add sub
	tick
	git commit -m a

	git checkout -b B main
	(cd sub
	 echo B > file
	 git add file
	 tick
	 git commit -m sub-b
	)
	git add sub
	tick
	git commit -m b
)

git init both-modify-union-attr
(cd both-modify-union-attr
  mkdir a && write_lines original 1 2 3 4 5 >a/x.f
  echo "a/* merge=union" >.gitattributes
  git add . && git commit -m "original"

  git branch A
  git branch B

  git checkout A
  write_lines A 1 2 3 4 5 6 >a/x.f
  git commit -am "change file"

  git checkout B
  write_lines B 1 2 3 4 5 7 >a/x.f
  git commit -am "change file differently"
)

git init both-modify-binary
(cd both-modify-binary
  mkdir a && printf '\x00 binary' >a/x.f
  git add . && git commit -m "original"

  git branch A
  git branch B

  git checkout A
  printf '\x00 A' >a/x.f
  git commit -am "change binary file"

  git checkout B
  printf '\x00 B' >a/x.f
  git commit -am "change binary file differently"
)

git init both-modify-file-with-binary-attr
(cd both-modify-file-with-binary-attr
  mkdir a && echo 'not binary' >a/x.f
  git add . && git commit -m "original"

  git branch A
  git branch B

  git checkout A
  echo 'A binary' >a/x.f
  git commit -am "change pseudo-binary file"

  git checkout B
  echo 'B binary' >a/x.f
  git commit -am "change pseudo-binary file differently"
)

git init big-file-merge
(cd big-file-merge
  git config --local core.bigFileThreshold 80
  mkdir a && write_lines original 1 2 3 4 5 >a/x.f
  git add . && git commit -m "original"

  git branch A
  git branch B

  git checkout A
  seq 30 >a/x.f
  git commit -am "turn normal file into big one (81 bytes)"
  git branch expected

  git checkout B
  write_lines 1 2 3 4 5 6 >a/x.f
  git commit -am "a normal but conflicting file change"
)

git init no-merge-base
(cd no-merge-base
  git checkout -b A
  echo "A" >content && git add . && git commit -m "content A"

  git checkout --orphan B
  echo "B" >content && git add . && git commit -m "content B"

  git checkout -b expectation
)

git init multiple-merge-bases
(cd multiple-merge-bases
  write_lines 1 2 3 4 5 >content
  git add . && git commit -m "initial"

  git branch A
  git branch B

  git checkout A
  write_lines 0 1 2 3 4 5 >content
  git commit -am "change in A" && git tag A1

  git checkout B
  write_lines 1 2 3 4 5 6 >content
  git commit -am "change in B" && git tag B1

  git checkout A
  git merge B1

  git checkout B
  git merge A1

  git checkout A
  write_lines 0 1 2 3 4 5 A >content
  git commit -am "conflicting in A"

  git checkout B
  git rm content
  write_lines 0 2 3 4 5 six >renamed
  git commit -m "rename in B"
)

git init rename-and-modification
(cd rename-and-modification
  mkdir a && write_lines original 1 2 3 4 5 >a/x.f
  git add . && git commit -m "original"

  git branch A
  git branch B

  git checkout A
  git mv a/x.f x.f
  git commit -am "move a/x.f to the top-level"

  git checkout B
  write_lines 1 2 3 4 5 6 >a/x.f
  git commit -am "changed a/x.f"
)

git init symlink-modification
(cd symlink-modification
  touch a b o
  ln -s o link
  git add . && git commit -m "original"

  git branch A
  git branch B

  git checkout A
  rm link && ln -s a link
  git commit -am "set link to point to 'a'"

  git checkout B
  rm link && ln -s b link
  git commit -am "set link to point to 'b'"
)

git init symlink-addition
(cd symlink-addition
  touch a b
  git add . && git commit -m "original without symlink"

  git branch A
  git branch B

  git checkout A
  ln -s a link && git add .
  git commit -m "new link to point to 'a'"

  git checkout B
  ln -s b link && git add .
  git commit -m "new link to point to 'b'"
)

git init type-change-to-symlink
(cd type-change-to-symlink
  touch a b link
  git add . && git commit -m "original without symlink"

  git branch A
  git branch B

  git checkout A
  git rm link
  ln -s a link && git add .
  git commit -m "new link to point to 'a'"

  git checkout B
  git rm link
  ln -s b link && git add .
  git commit -m "new link to point to 'b'"
)



baseline simple side-1-3-without-conflict side1 side3
baseline simple fast-forward side1 main
baseline simple no-change main main
baseline simple side-1-3-without-conflict-diff3 side1 side3
baseline simple side-1-2-various-conflicts side1 side2
baseline simple side-1-2-various-conflicts-diff3 side1 side2
baseline simple single-content-conflict side1 side4
baseline simple single-content-conflict-diff3 side1 side4
baseline simple tweak1-side2 tweak1 side2
baseline simple tweak1-side2-diff3 tweak1 side2
baseline simple side-1-unrelated side1 unrelated
baseline simple side-1-unrelated-diff3 side1 unrelated
baseline rename-delete A-B A B
baseline rename-delete A-similar A A
baseline rename-delete B-similar B B
baseline rename-add A-B A B
baseline rename-add A-B-diff3 A B
baseline rename-add-symlink A-B A B
baseline rename-add-symlink A-B-diff3 A B
baseline rename-rename-plus-content A-B A B
baseline rename-rename-plus-content A-B-diff3 A B
baseline rename-add-delete A-B A B
baseline rename-rename-delete-delete A-B A B
baseline super-1 A-B A B
baseline super-1 A-B-diff3 A B
baseline super-2 A-B A B
baseline super-2 A-B-diff3 A B

baseline rename-within-rename A-B-deviates A B "Git doesn't detect the rename-nesting so there is duplication - we achieve the optimal result"
baseline rename-within-rename-2 A-B-deviates A B "TBD: Right, something is different documentation was forgotten :/"
baseline conflicting-rename A-B A B
baseline conflicting-rename-2 A-B A B
baseline conflicting-rename-complex A-B A B "Git has different rename tracking which is why a-renamed/w disappears - it's still close enough"

baseline same-rename-different-mode A-B A B "Git works for the A/B case, but for B/A it forgets to set the executable bit"
baseline same-rename-different-mode A-B-diff3 A B "Git works for the A/B case, but for B/A it forgets to set the executable bit"
baseline renamed-symlink-with-conflict A-B A B
baseline added-file-changed-content-and-mode A-B A B "We improve on executable bit handling, but loose on diff quality as we are definitely missing some tweaks"

baseline type-change-and-renamed A-B A B
baseline change-and-delete A-B A B
baseline submodule-both-modify A-B A B "We can't handle submodules yet and just mark them as conflicting. This is planned to be improved."
baseline both-modify-union-attr A-B A B
baseline both-modify-union-attr A-B-diff3 A B
baseline both-modify-binary A-B A B
baseline both-modify-binary A-B A B
baseline both-modify-file-with-binary-attr A-B A B
baseline big-file-merge A-B A B "Git actually ignores core.bigFileThreshold during merging and tries a normal merge (or binary one) anyway. We don't ignore it and treat big files like binary files" \
                                no-reverse
baseline no-merge-base A-B A B
baseline no-merge-base A-B-diff3 A B

baseline multiple-merge-bases A-B A B
baseline multiple-merge-bases A-B-diff3 A B

baseline rename-and-modification A-B A B
baseline symlink-modification A-B A B
baseline symlink-addition A-B A B
baseline type-change-to-symlink A-B A B
