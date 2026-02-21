var minusImg = new Image();
minusImg.src = "./img/minus.gif";
var plusImg = new Image();
plusImg.src = "./img/plus.gif";

function collapseTree(num){
	if(document.getElementById == null) return;
	for(var i=1; i<=num; i++){
		expandContract(String(i));
	}
}

function expandContract(id){
	if(document.getElementById == null) return;
	for(var i=0; i<5; i++){
	  var prefix = '';
	  switch(i){
            case 0: prefix='schema'; break;
	    case 1: prefix='instance'; break;
	    case 2: prefix='validity'; break;
	    case 3: prefix='status'; break;
	    case 4: prefix='doc'; break;
	  }
	  
	  var img = document.getElementById(prefix+id+'Img');
	  if(img !=null){
	  	if(img.src == minusImg.src) img.src = plusImg.src;
	  	else img.src = minusImg.src;
	  }

	  if(document.getElementById(prefix+id+'Div')){
	  	var obj = document.getElementById(prefix+id+'Div').style;
	 	 if(obj.display == '') obj.display = "none";
	  	else obj.display = '';	  
	  }
	}
}